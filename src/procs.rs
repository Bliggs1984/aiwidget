//! Cheap process sampling.
//!
//! One `NtQuerySystemInformation(SystemProcessInformation)` call per tick gives pid, parent,
//! name, start time, CPU time, memory and I/O for every process (this is what Task Manager
//! uses). Command lines and working folders cost a process handle each, so they are read
//! once per process and cached.

use std::collections::HashMap;
use std::ffi::c_void;
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

#[repr(C)]
#[derive(Clone, Copy)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

/// Layout of SYSTEM_PROCESS_INFORMATION on 64-bit Windows.
#[repr(C)]
struct SpInfo {
    next_entry_offset: u32,
    number_of_threads: u32,
    working_set_private_size: i64,
    hard_fault_count: u32,
    number_of_threads_high_watermark: u32,
    cycle_time: u64,
    create_time: i64,
    user_time: i64,
    kernel_time: i64,
    image_name: UnicodeString,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
    handle_count: u32,
    session_id: u32,
    unique_process_key: usize,
    peak_virtual_size: usize,
    virtual_size: usize,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_page_count: usize,
    read_operation_count: i64,
    write_operation_count: i64,
    other_operation_count: i64,
    read_transfer_count: i64,
    write_transfer_count: i64,
    other_transfer_count: i64,
}

#[repr(C)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base_address: *mut c_void,
    affinity_mask: usize,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQuerySystemInformation(class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
    fn NtQueryInformationProcess(h: HANDLE, class: u32, info: *mut c_void, len: u32, ret_len: *mut u32) -> i32;
}

const SYSTEM_PROCESS_INFORMATION: u32 = 5;
const PROCESS_BASIC_INFORMATION: u32 = 0;
const PROCESS_CONSOLE_HOST_PROCESS: u32 = 49;
const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC0000004u32 as i32;
const STATUS_BUFFER_TOO_SMALL: i32 = 0xC0000023u32 as i32;
const STATUS_BUFFER_OVERFLOW: i32 = 0x80000005u32 as i32;

#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// FILETIME units (100 ns since 1601).
    pub create_time: i64,
    /// Percent of the whole machine (all cores = 100).
    pub cpu: f32,
    /// Private working set, bytes (Task Manager's "Memory" column).
    pub mem: u64,
    /// Bytes per second of read+write I/O.
    pub io: f64,
}

struct Prev {
    create_time: i64,
    cpu_time: i64,
    io_bytes: i64,
}

pub struct Sampler {
    buf: Vec<u64>,
    prev: HashMap<u32, Prev>,
    last: Option<Instant>,
    ncpu: f64,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            buf: vec![0u64; 64 * 1024],
            prev: HashMap::new(),
            last: None,
            ncpu: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64,
        }
    }

    pub fn sample(&mut self) -> Vec<Proc> {
        // Fill the buffer, growing it if the process list got bigger.
        loop {
            let mut needed = 0u32;
            let st = unsafe {
                NtQuerySystemInformation(
                    SYSTEM_PROCESS_INFORMATION,
                    self.buf.as_mut_ptr() as *mut c_void,
                    (self.buf.len() * 8) as u32,
                    &mut needed,
                )
            };
            if st == STATUS_INFO_LENGTH_MISMATCH {
                self.buf.resize((needed as usize).div_ceil(8) + 8 * 1024, 0);
                continue;
            }
            if st < 0 {
                return Vec::new();
            }
            break;
        }

        let now = Instant::now();
        let dt = self.last.map(|l| now.duration_since(l).as_secs_f64()).unwrap_or(0.0);
        self.last = Some(now);

        let mut out = Vec::with_capacity(400);
        let mut next_prev = HashMap::with_capacity(self.prev.len().max(400));
        let mut off = 0usize;
        loop {
            // SAFETY: the kernel fills the buffer with a chain of 8-byte aligned SpInfo entries
            // whose offsets stay inside the buffer (a Vec<u64>, so the base is 8-aligned).
            let e = unsafe { &*((self.buf.as_ptr() as *const u8).add(off) as *const SpInfo) };
            let pid = e.unique_process_id as u32;
            let name = if e.image_name.buffer.is_null() || e.image_name.length == 0 {
                if pid == 0 { "Idle".to_string() } else { "System".to_string() }
            } else {
                let s = unsafe {
                    std::slice::from_raw_parts(e.image_name.buffer, (e.image_name.length / 2) as usize)
                };
                String::from_utf16_lossy(s)
            };
            let cpu_time = e.user_time + e.kernel_time;
            let io_bytes = e.read_transfer_count + e.write_transfer_count;
            let (cpu, io) = match self.prev.get(&pid) {
                Some(p) if p.create_time == e.create_time && dt > 0.0 => {
                    let dcpu = (cpu_time - p.cpu_time).max(0) as f64 / 1e7; // seconds
                    let dio = (io_bytes - p.io_bytes).max(0) as f64;
                    ((dcpu / dt / self.ncpu * 100.0) as f32, dio / dt)
                }
                _ => (0.0, 0.0),
            };
            next_prev.insert(pid, Prev { create_time: e.create_time, cpu_time, io_bytes });
            if pid != 0 {
                out.push(Proc {
                    pid,
                    ppid: e.inherited_from_unique_process_id as u32,
                    name,
                    create_time: e.create_time,
                    cpu,
                    mem: e.working_set_private_size.max(0) as u64,
                    io,
                });
            }
            if e.next_entry_offset == 0 {
                break;
            }
            off += e.next_entry_offset as usize;
            if off + std::mem::size_of::<SpInfo>() > self.buf.len() * 8 {
                break;
            }
        }
        self.prev = next_prev;
        out
    }
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Full command line of a process, or None if access is denied.
pub fn read_cmdline(pid: u32) -> Option<String> {
    let h = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? });
    let mut buf: Vec<u64> = vec![0; 512]; // u64 for alignment; 4 KB
    loop {
        let mut needed = 0u32;
        let bytes = (buf.len() * 8) as u32;
        let st = unsafe {
            NtQueryInformationProcess(
                h.0,
                PROCESS_COMMAND_LINE_INFORMATION,
                buf.as_mut_ptr() as *mut c_void,
                bytes,
                &mut needed,
            )
        };
        if (st == STATUS_INFO_LENGTH_MISMATCH || st == STATUS_BUFFER_TOO_SMALL || st == STATUS_BUFFER_OVERFLOW)
            && needed > bytes
            && needed < 1 << 20
        {
            buf.resize((needed as usize).div_ceil(8) + 1, 0);
            continue;
        }
        if st < 0 {
            return None;
        }
        break;
    }
    let us = unsafe { &*(buf.as_ptr() as *const UnicodeString) };
    if us.buffer.is_null() || us.length == 0 {
        return Some(String::new());
    }
    let s = unsafe { std::slice::from_raw_parts(us.buffer, (us.length / 2) as usize) };
    Some(String::from_utf16_lossy(s))
}

/// Current working folder of a 64-bit process (reads its PEB). None if not allowed.
pub fn read_cwd(pid: u32) -> Option<String> {
    let h = Handle(unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()? });
    let mut pbi: ProcessBasicInformation = unsafe { std::mem::zeroed() };
    let st = unsafe {
        NtQueryInformationProcess(
            h.0,
            PROCESS_BASIC_INFORMATION,
            &mut pbi as *mut _ as *mut c_void,
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    if st < 0 || pbi.peb_base_address.is_null() {
        return None;
    }
    let read = |addr: usize, dst: *mut c_void, len: usize| -> bool {
        unsafe { ReadProcessMemory(h.0, addr as *const c_void, dst, len, None).is_ok() }
    };
    // PEB.ProcessParameters at +0x20; RTL_USER_PROCESS_PARAMETERS.CurrentDirectory.DosPath at +0x38.
    let mut params: usize = 0;
    if !read(pbi.peb_base_address as usize + 0x20, &mut params as *mut _ as *mut c_void, 8) || params == 0 {
        return None;
    }
    let mut us = UnicodeString { length: 0, maximum_length: 0, buffer: std::ptr::null_mut() };
    if !read(params + 0x38, &mut us as *mut _ as *mut c_void, std::mem::size_of::<UnicodeString>()) {
        return None;
    }
    // The length comes from the other process. An odd one would overrun the buffer below.
    if us.length == 0 || us.length > 4096 || us.length % 2 != 0 || us.buffer.is_null() {
        return None;
    }
    let mut chars = vec![0u16; (us.length / 2) as usize];
    if !read(us.buffer as usize, chars.as_mut_ptr() as *mut c_void, us.length as usize) {
        return None;
    }
    Some(String::from_utf16_lossy(&chars))
}

/// Largest environment block we will read, in bytes. Real ones are far smaller.
const MAX_ENV_BYTES: usize = 1 << 20;

/// What a process's inherited environment says about where it came from.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct EnvInfo {
    /// `CLAUDE_PID`: Claude Code sets this for everything it starts.
    pub agent_pid: Option<u32>,
    /// `MSYSTEM` is set, so this came through Git Bash. Git Bash's own programs keep only a
    /// handful of variables where we can read them, but programs they start get the full set.
    pub git_bash: bool,
}

/// The console host process (conhost.exe) a process is attached to. None if it has no console.
pub fn read_console_host(pid: u32) -> Option<u32> {
    let h = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? });
    let mut host: usize = 0;
    let st = unsafe {
        NtQueryInformationProcess(
            h.0,
            PROCESS_CONSOLE_HOST_PROCESS,
            &mut host as *mut _ as *mut c_void,
            std::mem::size_of::<usize>() as u32,
            std::ptr::null_mut(),
        )
    };
    // The low two bits are flags, not part of the pid.
    let host = (host & !3) as u32;
    if st < 0 || host == 0 { None } else { Some(host) }
}

/// Reads a process's environment block from its PEB. None if not allowed or not ready yet.
pub fn read_env_info(pid: u32) -> Option<EnvInfo> {
    read_env_block(pid).map(|b| env_info(&b))
}

fn read_env_block(pid: u32) -> Option<Vec<u16>> {
    let h = Handle(unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid).ok()? });
    let mut pbi: ProcessBasicInformation = unsafe { std::mem::zeroed() };
    let st = unsafe {
        NtQueryInformationProcess(
            h.0,
            PROCESS_BASIC_INFORMATION,
            &mut pbi as *mut _ as *mut c_void,
            std::mem::size_of::<ProcessBasicInformation>() as u32,
            std::ptr::null_mut(),
        )
    };
    if st < 0 || pbi.peb_base_address.is_null() {
        return None;
    }
    let read = |addr: usize, dst: *mut c_void, len: usize| -> bool {
        unsafe { ReadProcessMemory(h.0, addr as *const c_void, dst, len, None).is_ok() }
    };
    // PEB.ProcessParameters at +0x20; RTL_USER_PROCESS_PARAMETERS.Environment at +0x80 and
    // EnvironmentSize at +0x3F0.
    let (mut params, mut env, mut size) = (0usize, 0usize, 0usize);
    if !read(pbi.peb_base_address as usize + 0x20, &mut params as *mut _ as *mut c_void, 8) || params == 0 {
        return None;
    }
    if !read(params + 0x80, &mut env as *mut _ as *mut c_void, 8) || env == 0 {
        return None;
    }
    if !read(params + 0x3F0, &mut size as *mut _ as *mut c_void, 8) || size == 0 {
        return None;
    }
    let mut block = vec![0u16; size.min(MAX_ENV_BYTES) / 2];
    if !read(env, block.as_mut_ptr() as *mut c_void, block.len() * 2) {
        return None;
    }
    Some(block)
}

/// Reads the variables we care about from a Windows environment block (NUL-separated UTF-16).
pub fn env_info(block: &[u16]) -> EnvInfo {
    fn value<'a>(var: &'a [u16], name: &[u8]) -> Option<&'a [u16]> {
        let named = var.len() >= name.len()
            && var.iter().zip(name).all(|(&c, &n)| c < 128 && (c as u8).to_ascii_uppercase() == n);
        named.then(|| &var[name.len()..])
    }
    let mut info = EnvInfo::default();
    for var in block.split(|&c| c == 0) {
        if let Some(v) = value(var, b"CLAUDE_PID=") {
            info.agent_pid = String::from_utf16_lossy(v).trim().parse().ok();
        } else if value(var, b"MSYSTEM=").is_some_and(|v| !v.is_empty()) {
            info.git_bash = true;
        }
    }
    info
}

/// Current time in FILETIME units, for computing process ages.
pub fn now_filetime() -> i64 {
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(vars: &[&str]) -> Vec<u16> {
        let mut b = Vec::new();
        for v in vars {
            b.extend(v.encode_utf16());
            b.push(0);
        }
        b.push(0);
        b
    }

    #[test]
    fn reads_variables_from_an_environment_block() {
        let i = env_info(&block(&["PATH=C:\\x", "CLAUDE_PID=52168", "MSYSTEM=MINGW64", "Z=1"]));
        assert_eq!(i, EnvInfo { agent_pid: Some(52168), git_bash: true });
        assert_eq!(env_info(&block(&["claude_pid=7"])).agent_pid, Some(7));
    }

    #[test]
    fn ignores_lookalikes_and_bad_values() {
        let none = EnvInfo::default();
        assert_eq!(env_info(&block(&["NOT_CLAUDE_PID=5", "CLAUDE_PIDS=6", "MSYSTEMS=x"])), none);
        assert_eq!(env_info(&block(&["CLAUDE_PID=abc"])), none);
        assert_eq!(env_info(&block(&["CLAUDE_PID=", "MSYSTEM="])), none);
        assert_eq!(env_info(&[]), none);
    }

    /// Guards the PEB offsets: a real child must report the values we gave it.
    #[test]
    fn reads_the_values_from_a_real_process() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "ping -n 3 127.0.0.1 >nul"])
            .env("CLAUDE_PID", "4242")
            .env("MSYSTEM", "MINGW64")
            .spawn()
            .unwrap();
        let got = read_env_info(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(got, Some(EnvInfo { agent_pid: Some(4242), git_bash: true }));
    }
}
