use core::sync::atomic::Ordering;

use axerrno::{LinuxError, LinuxResult};
use axtask::current;
use linux_raw_sys::general::{
    FUTEX_CMD_MASK, FUTEX_CMP_REQUEUE, FUTEX_REQUEUE, FUTEX_WAIT, FUTEX_WAIT_BITSET, FUTEX_WAKE,
    FUTEX_WAKE_BITSET, robust_list, robust_list_head, timespec,
};
use starry_core::{
    futex::FutexKey,
    task::{StarryTaskExt, ThreadData, get_thread},
};

use crate::{
    ptr::{UserConstPtr, UserPtr, nullable},
    time::TimeValueLike,
};

pub const ROBUST_LIST_LIMIT: usize = 2048;

fn assert_unsigned(value: u32) -> LinuxResult<u32> {
    if (value as i32) < 0 {
        Err(LinuxError::EINVAL)
    } else {
        Ok(value)
    }
}

pub fn sys_futex(
    uaddr: UserConstPtr<u32>,
    futex_op: u32,
    value: u32,
    timeout: UserConstPtr<timespec>,
    uaddr2: UserPtr<u32>,
    value3: u32,
) -> LinuxResult<isize> {
    info!("futex {:?} {} {}", uaddr.address(), futex_op, value);

    let curr = current();

    let key = FutexKey::new_current(uaddr.address().as_usize());
    let proc_data = StarryTaskExt::of(&curr).process_data();
    let futex_table = proc_data.futex_table_for(&key);
    let command = futex_op & (FUTEX_CMD_MASK as u32);
    match command {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            let uaddr_ref = uaddr.get_as_ref()?;

            // Fast path
            if *uaddr_ref != value {
                return Err(LinuxError::EAGAIN);
            }

            let timeout = nullable!(timeout.get_as_ref())?
                .map(|ts| ts.try_into_time_value())
                .transpose()?;

            // This function is called with the lock to run queue being held by
            // us, and thus we need to check FOR ONCE if the value has changed.
            // If so, we shall skip waiting and return EAGAIN; otherwise, we
            // return false to start waiting and return true for subsequent
            // calls.
            let mut first_call = true;
            let mut mismatches = false;
            let condition = || {
                if first_call {
                    mismatches = *uaddr_ref != value;
                    first_call = false;
                    mismatches
                } else {
                    true
                }
            };

            let futex = futex_table.get_or_insert(&key);

            if command == FUTEX_WAIT_BITSET {
                StarryTaskExt::of(&curr)
                    .thread_data()
                    .futex_bitset
                    .store(value3, Ordering::SeqCst);
            }

            if let Some(timeout) = timeout {
                if futex.wq.wait_timeout_until(timeout, condition) {
                    return Err(LinuxError::ETIMEDOUT);
                }
            } else {
                futex.wq.wait_until(condition);
            }
            if mismatches {
                return Err(LinuxError::EAGAIN);
            }

            if futex.owner_dead.swap(false, Ordering::SeqCst) {
                Err(LinuxError::EOWNERDEAD)
            } else {
                Ok(0)
            }
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let futex = futex_table.get(&key);
            let mut count = 0;
            if let Some(futex) = futex {
                futex.wq.notify_all_if(false, |task| {
                    if count >= value {
                        false
                    } else {
                        let wake = if command == FUTEX_WAKE_BITSET {
                            let bitset = StarryTaskExt::of(task)
                                .thread_data()
                                .futex_bitset
                                .load(Ordering::SeqCst);
                            (bitset & value3) != 0
                        } else {
                            true
                        };
                        count += wake as u32;
                        wake
                    }
                });
            }
            axtask::yield_now();
            Ok(count as isize)
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            assert_unsigned(value)?;
            if command == FUTEX_CMP_REQUEUE && *uaddr.get_as_ref()? != value3 {
                return Err(LinuxError::EAGAIN);
            }
            let value2 = assert_unsigned(timeout.address().as_usize() as u32)?;

            let futex = futex_table.get(&key);
            let key2 = FutexKey::new_current(uaddr2.address().as_usize());
            let futex2 = proc_data.futex_table_for(&key2).get_or_insert(&key2);

            let mut count = 0;
            if let Some(futex) = futex {
                for _ in 0..value {
                    if !futex.wq.notify_one(false) {
                        break;
                    }
                    count += 1;
                }
                if count == value as isize {
                    count += futex.wq.requeue(value2 as usize, &futex2.wq) as isize;
                }
            }
            Ok(count)
        }
        _ => Err(LinuxError::ENOSYS),
    }
}

pub fn sys_get_robust_list(
    tid: u32,
    head: UserPtr<UserConstPtr<robust_list_head>>,
    size: UserPtr<usize>,
) -> LinuxResult<isize> {
    let thr = if tid == 0 {
        StarryTaskExt::of(&current()).thread.clone()
    } else {
        get_thread(tid)?
    };
    *head.get_as_mut()? = thr
        .data::<ThreadData>()
        .unwrap()
        .robust_list_head
        .load(Ordering::SeqCst)
        .into();
    *size.get_as_mut()? = size_of::<robust_list_head>();

    Ok(0)
}

pub fn sys_set_robust_list(
    head: UserConstPtr<robust_list_head>,
    size: usize,
) -> LinuxResult<isize> {
    if size != size_of::<robust_list_head>() {
        return Err(LinuxError::EINVAL);
    }
    StarryTaskExt::of(&current())
        .thread_data()
        .robust_list_head
        .store(head.address().as_usize(), Ordering::SeqCst);

    Ok(0)
}

fn handle_futex_death(entry: *mut robust_list, offset: i64) -> LinuxResult<()> {
    let address = (entry as u64)
        .checked_add_signed(offset)
        .ok_or(LinuxError::EINVAL)?;
    let address: usize = address.try_into().map_err(|_| LinuxError::EINVAL)?;
    let key = FutexKey::new_current(address);

    let curr = current();
    let futex_table = &StarryTaskExt::of(&curr)
        .process_data()
        .futex_table_for(&key);

    let Some(futex) = futex_table.get(&key) else {
        return Ok(());
    };
    futex.owner_dead.store(true, Ordering::SeqCst);
    futex.wq.notify_one(false);
    Ok(())
}

pub fn exit_robust_list(head: &mut robust_list_head) -> LinuxResult<()> {
    // Reference: https://elixir.bootlin.com/linux/v6.13.6/source/kernel/futex/core.c#L777

    let mut limit = ROBUST_LIST_LIMIT;

    let mut entry = head.list.next;
    let offset = head.futex_offset;
    let pending = head.list_op_pending;

    while !core::ptr::eq(entry, &head.list) {
        let next_entry = UserPtr::from(entry).get_as_mut()?.next;
        if entry != pending {
            handle_futex_death(entry, offset)?;
        }
        entry = next_entry;

        limit -= 1;
        if limit == 0 {
            return Err(LinuxError::ELOOP);
        }
        axtask::yield_now();
    }

    Ok(())
}
