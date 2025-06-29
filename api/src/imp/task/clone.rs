use core::sync::atomic::{AtomicU32, Ordering};

use alloc::sync::Arc;
use axerrno::{LinuxError, LinuxResult};
use axfs_ng::FS_CONTEXT;
use axhal::arch::{TrapFrame, UspaceContext};
use axprocess::Pid;
use axsignal::Signo;
use axsync::Mutex;
use axtask::{TaskExtProxy, current};
use bitflags::bitflags;
use linux_raw_sys::general::*;
use starry_core::{
    mm::copy_from_kernel,
    task::{ProcessData, StarryTaskExt, ThreadData, add_thread_to_table, new_user_task},
};

use crate::{file::FD_TABLE, ptr::UserPtr};

bitflags! {
    /// Options for use with [`sys_clone`].
    #[derive(Debug, Clone, Copy, Default)]
    struct CloneFlags: u32 {
        /// The calling process and the child process run in the same
        /// memory space.
        const VM = CLONE_VM;
        /// The caller and the child process share the same  filesystem
        /// information.
        const FS = CLONE_FS;
        /// The calling process and the child process share the same file
        /// descriptor table.
        const FILES = CLONE_FILES;
        /// The calling process and the child process share the same table
        /// of signal handlers.
        const SIGHAND = CLONE_SIGHAND;
        /// If the calling process is being traced, then trace the child
        /// also.
        const PTRACE = CLONE_PTRACE;
        /// The execution of the calling process is suspended until the
        /// child releases its virtual memory resources via a call to
        /// execve(2) or _exit(2) (as with vfork(2)).
        const VFORK = CLONE_VFORK;
        /// The parent of the new child  (as returned by getppid(2))
        /// will be the same as that of the calling process.
        const PARENT = CLONE_PARENT;
        /// The child is placed in the same thread group as the calling
        /// process.
        const THREAD = CLONE_THREAD;
        /// The cloned child is started in a new mount namespace.
        const NEWNS = CLONE_NEWNS;
        /// The child and the calling process share a single list of System
        /// V semaphore adjustment values
        const SYSVSEM = CLONE_SYSVSEM;
        /// The TLS (Thread Local Storage) descriptor is set to tls.
        const SETTLS = CLONE_SETTLS;
        /// Store the child thread ID in the parent's memory.
        const PARENT_SETTID = CLONE_PARENT_SETTID;
        /// Clear (zero) the child thread ID in child memory when the child
        /// exits, and do a wakeup on the futex at that address.
        const CHILD_CLEARTID = CLONE_CHILD_CLEARTID;
        /// A tracing process cannot force `CLONE_PTRACE` on this child
        /// process.
        const UNTRACED = CLONE_UNTRACED;
        /// Store the child thread ID in the child's memory.
        const CHILD_SETTID = CLONE_CHILD_SETTID;
        /// Create the process in a new cgroup namespace.
        const NEWCGROUP = CLONE_NEWCGROUP;
        /// Create the process in a new UTS namespace.
        const NEWUTS = CLONE_NEWUTS;
        /// Create the process in a new IPC namespace.
        const NEWIPC = CLONE_NEWIPC;
        /// Create the process in a new user namespace.
        const NEWUSER = CLONE_NEWUSER;
        /// Create the process in a new PID namespace.
        const NEWPID = CLONE_NEWPID;
        /// Create the process in a new network namespace.
        const NEWNET = CLONE_NEWNET;
        /// The new process shares an I/O context with the calling process.
        const IO = CLONE_IO;
    }
}

pub fn sys_clone(
    tf: &TrapFrame,
    flags: u32,
    stack: usize,
    parent_tid: usize,
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))] child_tid: usize,
    tls: usize,
    #[cfg(not(any(target_arch = "x86_64", target_arch = "loongarch64")))] child_tid: usize,
) -> LinuxResult<isize> {
    const FLAG_MASK: u32 = 0xff;
    let exit_signal = flags & FLAG_MASK;
    let flags = CloneFlags::from_bits_truncate(flags & !FLAG_MASK);

    info!(
        "sys_clone <= flags: {:?}, exit_signal: {}, stack: {:#x}, ptid: {:#x}, ctid: {:#x}, tls: {:#x}",
        flags, exit_signal, stack, parent_tid, child_tid, tls
    );

    if exit_signal != 0 && flags.contains(CloneFlags::THREAD | CloneFlags::PARENT) {
        return Err(LinuxError::EINVAL);
    }
    if flags.contains(CloneFlags::THREAD) && !flags.contains(CloneFlags::VM | CloneFlags::SIGHAND) {
        return Err(LinuxError::EINVAL);
    }
    let exit_signal = Signo::from_repr(exit_signal as u8);

    let mut new_uctx = UspaceContext::from(tf);
    if stack != 0 {
        new_uctx.set_sp(stack);
    }
    if flags.contains(CloneFlags::SETTLS) {
        new_uctx.set_tls(tls);
    }
    new_uctx.set_retval(0);

    let set_child_tid = if flags.contains(CloneFlags::CHILD_SETTID) {
        Some(UserPtr::<u32>::from(child_tid).get_as_mut()?)
    } else {
        None
    };

    let curr = current();
    let ext = StarryTaskExt::of(&curr);
    let mut new_task = new_user_task(curr.name(), new_uctx, set_child_tid);

    let tid = new_task.id().as_u64() as Pid;
    if flags.contains(CloneFlags::PARENT_SETTID) {
        *UserPtr::<Pid>::from(parent_tid).get_as_mut()? = tid;
    }

    let process = if flags.contains(CloneFlags::THREAD) {
        new_task
            .ctx_mut()
            .set_page_table_root(ext.process_data().aspace.lock().page_table_root());

        ext.thread.process()
    } else {
        let parent = if flags.contains(CloneFlags::PARENT) {
            ext.thread.process().parent().ok_or(LinuxError::EINVAL)?
        } else {
            ext.thread.process().clone()
        };
        let builder = parent.fork(tid);

        let aspace = if flags.contains(CloneFlags::VM) {
            ext.process_data().aspace.clone()
        } else {
            let mut aspace = ext.process_data().aspace.lock();
            let mut aspace = aspace.try_clone()?;
            copy_from_kernel(&mut aspace)?;
            Arc::new(Mutex::new(aspace))
        };
        new_task
            .ctx_mut()
            .set_page_table_root(aspace.lock().page_table_root());

        let signal_actions = if flags.contains(CloneFlags::SIGHAND) {
            parent
                .data::<ProcessData>()
                .map_or_else(Arc::default, |it| it.signal.actions.clone())
        } else {
            Arc::default()
        };
        let mut process_data = ProcessData::new(
            ext.process_data().exe_path.read().clone(),
            aspace,
            signal_actions,
            exit_signal,
        );
        process_data.umask = AtomicU32::new(ext.process_data().umask.load(Ordering::SeqCst));

        {
            let mut scope = process_data.scope.write();
            if flags.contains(CloneFlags::FILES) {
                FD_TABLE.scope_mut(&mut scope).clone_from(&FD_TABLE);
            } else {
                FD_TABLE
                    .scope_mut(&mut scope)
                    .write()
                    .clone_from(&FD_TABLE.read());
            }

            if flags.contains(CloneFlags::FS) {
                FS_CONTEXT.scope_mut(&mut scope).clone_from(&FS_CONTEXT);
            } else {
                FS_CONTEXT
                    .scope_mut(&mut scope)
                    .lock()
                    .clone_from(&FS_CONTEXT.lock());
            }
        }
        &builder.data(process_data).build()
    };

    let thread_data = ThreadData::new(process.data().unwrap());
    if flags.contains(CloneFlags::CHILD_CLEARTID) {
        thread_data.set_clear_child_tid(child_tid);
    }

    let thread = process.new_thread(tid).data(thread_data).build();
    add_thread_to_table(&thread);
    *new_task.task_ext_mut() = Some(unsafe { TaskExtProxy::from_impl(StarryTaskExt::new(thread)) });
    let task = axtask::spawn_task(new_task);
    StarryTaskExt::of(&task)
        .thread_data()
        .init_task(|| Arc::downgrade(&task));

    Ok(tid as _)
}

pub fn sys_fork(tf: &TrapFrame) -> LinuxResult<isize> {
    sys_clone(tf, SIGCHLD, 0, 0, 0, 0)
}
