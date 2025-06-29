use core::any::Any;

use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::btree_map::BTreeMap,
    string::String,
    sync::Arc,
};
use axfs_ng_vfs::{
    DirEntry, DirEntrySink, DirNode, DirNodeOps, FileNode, FilesystemOps, Metadata, MetadataUpdate,
    NodeOps, NodePermission, NodeType, Reference, VfsError, VfsResult, WeakDirEntry,
    path::{DOT, DOTDOT},
};
use inherit_methods_macro::inherit_methods;
use lock_api::RawMutex;

use crate::vfs::simple::{DirMaker, NodeOpsMux, SimpleFs, SimpleFsNode};

pub trait SimpleDirOps<M>: Send + Sync {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a>;
    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux<M>>;

    fn is_cacheable(&self) -> bool {
        true
    }

    fn chain<N: SimpleDirOps<M>>(self, other: N) -> ChainedDirOps<Self, N>
    where
        Self: Sized,
    {
        ChainedDirOps(self, other)
    }
}
impl<M: RawMutex> SimpleDirOps<M> for DirMapping<M> {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.0.keys().map(|s| s.as_str().into()))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux<M>> {
        self.0.get(name).cloned().ok_or(VfsError::ENOENT)
    }
}

pub struct DirMapping<M>(BTreeMap<String, NodeOpsMux<M>>);
impl<M: RawMutex> DirMapping<M> {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn add(&mut self, name: impl Into<String>, ops: impl Into<NodeOpsMux<M>>) {
        self.0.insert(name.into(), ops.into());
    }
}

pub struct ChainedDirOps<A, B>(A, B);
impl<M: RawMutex, A: SimpleDirOps<M>, B: SimpleDirOps<M>> SimpleDirOps<M> for ChainedDirOps<A, B> {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(self.0.child_names().chain(self.1.child_names()))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux<M>> {
        match self.0.lookup_child(name) {
            Ok(ops) => Ok(ops),
            Err(VfsError::ENOENT) => self.1.lookup_child(name),
            Err(e) => Err(e),
        }
    }

    fn is_cacheable(&self) -> bool {
        // TODO: If one of the ops is not cacheable while the other is, the
        // behavior is undefined.
        self.0.is_cacheable() && self.1.is_cacheable()
    }
}

pub struct SimpleDir<M: RawMutex, O> {
    node: SimpleFsNode<M>,
    this: WeakDirEntry<M>,
    ops: Arc<O>,
}
impl<M: RawMutex + Send + Sync + 'static, O: SimpleDirOps<M> + 'static> SimpleDir<M, O> {
    fn new(node: SimpleFsNode<M>, ops: Arc<O>, this: WeakDirEntry<M>) -> Arc<Self> {
        Arc::new(Self { node, this, ops })
    }

    pub fn new_maker(fs: Arc<SimpleFs<M>>, ops: Arc<O>) -> DirMaker<M> {
        Arc::new(move |this| {
            SimpleDir::new(
                SimpleFsNode::new(
                    fs.clone(),
                    NodeType::Directory,
                    NodePermission::from_bits_truncate(0o755),
                ),
                ops.clone(),
                this,
            )
        })
    }
}

#[inherit_methods(from = "self.node")]
impl<M: RawMutex + Send + Sync + 'static, O: SimpleDirOps<M> + 'static> NodeOps<M>
    for SimpleDir<M, O>
{
    fn inode(&self) -> u64;
    fn metadata(&self) -> VfsResult<Metadata>;
    fn update_metadata(&self, update: MetadataUpdate) -> VfsResult<()>;
    fn filesystem(&self) -> &dyn FilesystemOps<M>;
    fn sync(&self, data_only: bool) -> VfsResult<()>;
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl<M: RawMutex + Send + Sync + 'static, O: SimpleDirOps<M> + 'static> DirNodeOps<M>
    for SimpleDir<M, O>
{
    fn read_dir(&self, offset: u64, sink: &mut dyn DirEntrySink) -> VfsResult<usize> {
        let children = [DOT, DOTDOT]
            .into_iter()
            .map(Cow::Borrowed)
            .chain(self.ops.child_names());

        let this_entry = self.this.upgrade().unwrap();
        let this_dir = this_entry.as_dir()?;

        let mut count = 0;
        for (i, name) in children.enumerate().skip(offset as usize) {
            let metadata = match name.as_ref() {
                DOT => this_entry.metadata(),
                DOTDOT => this_entry
                    .parent()
                    .map_or_else(|| this_entry.metadata(), |parent| parent.metadata()),
                other => {
                    let entry = this_dir.lookup(other)?;
                    entry.metadata()
                }
            }?;
            if !sink.accept(&name, metadata.inode, metadata.node_type, i as u64 + 1) {
                break;
            }
            count += 1;
        }

        Ok(count)
    }

    fn lookup(&self, name: &str) -> VfsResult<DirEntry<M>> {
        let ops = self.ops.lookup_child(name)?;
        let reference = Reference::new(self.this.upgrade(), name.to_owned());
        Ok(match ops {
            NodeOpsMux::Dir(maker) => {
                DirEntry::new_dir(|this| DirNode::new(maker(this)), reference)
            }
            NodeOpsMux::File(ops) => {
                let node_type = ops.metadata()?.node_type;
                DirEntry::new_file(FileNode::new(ops.clone()), node_type, reference)
            }
        })
    }

    fn is_cacheable(&self) -> bool {
        self.ops.is_cacheable()
    }

    fn create(
        &self,
        _name: &str,
        _node_type: NodeType,
        _permission: NodePermission,
    ) -> VfsResult<DirEntry<M>> {
        Err(VfsError::EPERM)
    }

    fn link(&self, _name: &str, _node: &DirEntry<M>) -> VfsResult<DirEntry<M>> {
        Err(VfsError::EPERM)
    }

    fn unlink(&self, _name: &str) -> VfsResult<()> {
        Err(VfsError::EPERM)
    }

    fn rename(&self, _src_name: &str, _dst_dir: &DirNode<M>, _dst_name: &str) -> VfsResult<()> {
        Err(VfsError::EPERM)
    }
}
