use std::ffi::{CStr, c_char, c_int, c_void};
use std::mem;
use std::ptr;
use std::sync::OnceLock;

use crate::dynamic_linker::AndroidDlExtInfo;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;

const DT_NULL: isize = 0;
const DT_HASH: isize = 4;
const DT_STRTAB: isize = 5;
const DT_SYMTAB: isize = 6;
const DT_GNU_HASH: isize = 0x6ffffef5;

const API_N: i32 = 24;
const API_N_MR1: i32 = 25;
const API_O: i32 = 26;
const API_P: i32 = 28;

const LINKER_DLOPEN_EXT_N: &str = "__dl__ZL10dlopen_extPKciPK17android_dlextinfoPv";
const LINKER_DO_DLOPEN_N: &str = "__dl__Z9do_dlopenPKciPK17android_dlextinfoPv";
const LINKER_DLOPEN_O: &str = "__dl__Z8__dlopenPKciPKv";
const LINKER_LOADER_DLOPEN_P: &str = "__loader_dlopen";

type DlopenAndroidN =
    unsafe extern "C" fn(*const c_char, c_int, *const AndroidDlExtInfo, *mut c_void) -> *mut c_void;
type DlopenAndroidO = unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> *mut c_void;

unsafe extern "C" {
    fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
}

#[cfg(target_pointer_width = "64")]
type ElfPhdr = libc::Elf64_Phdr;
#[cfg(target_pointer_width = "32")]
type ElfPhdr = libc::Elf32_Phdr;

#[cfg(target_pointer_width = "64")]
type ElfAddr = u64;
#[cfg(target_pointer_width = "32")]
type ElfAddr = u32;

#[cfg(target_pointer_width = "64")]
#[repr(C)]
struct ElfDyn {
    d_tag: i64,
    d_val: u64,
}

#[cfg(target_pointer_width = "32")]
#[repr(C)]
struct ElfDyn {
    d_tag: i32,
    d_val: u32,
}

#[cfg(target_pointer_width = "64")]
#[repr(C)]
struct ElfSym {
    st_name: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
    st_value: u64,
    st_size: u64,
}

#[cfg(target_pointer_width = "32")]
#[repr(C)]
struct ElfSym {
    st_name: u32,
    st_value: u32,
    st_size: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
}

#[derive(Clone, Copy)]
struct LinkerState {
    dlopen_symbol: *mut c_void,
    callers: [*mut c_void; 3],
    api_level: i32,
}

// LinkerState contains immutable process addresses discovered once.
unsafe impl Send for LinkerState {}
unsafe impl Sync for LinkerState {}

pub(crate) fn force_dlopen(path: &CStr) -> Result<*mut c_void, &'static str> {
    let api_level = device_api_level();
    if api_level < API_N {
        return Err("private linker force-load is not needed before Android N");
    }

    let state = linker_state().ok_or("private linker symbols unavailable")?;
    if state.callers.iter().all(|caller| caller.is_null()) {
        return Err("private linker caller addresses unavailable");
    }

    let handle = if (API_N..=API_N_MR1).contains(&state.api_level) {
        force_dlopen_android_n(path, &state)
    } else {
        force_dlopen_android_o_or_newer(path, &state)
    };
    if handle.is_null() {
        Err("private linker force-load returned null")
    } else {
        Ok(handle)
    }
}

fn linker_state() -> Option<LinkerState> {
    static STATE: OnceLock<Option<LinkerState>> = OnceLock::new();
    *STATE.get_or_init(discover_linker_state)
}

fn discover_linker_state() -> Option<LinkerState> {
    let api_level = device_api_level();
    let mut scan = PhdrScan::new(api_level);
    unsafe {
        libc::dl_iterate_phdr(Some(scan_phdrs), (&mut scan as *mut PhdrScan).cast());
    }

    let linker = scan.linker?;
    let dlopen_symbol = find_loader_symbol(linker, api_level)?;
    Some(LinkerState {
        dlopen_symbol,
        callers: scan.callers,
        api_level,
    })
}

fn force_dlopen_android_n(path: &CStr, state: &LinkerState) -> *mut c_void {
    let dlopen: DlopenAndroidN = unsafe { mem::transmute(state.dlopen_symbol) };
    for caller in state.callers {
        if caller.is_null() {
            continue;
        }
        let handle = unsafe { dlopen(path.as_ptr(), libc::RTLD_NOW, ptr::null(), caller) };
        if !handle.is_null() {
            return handle;
        }
    }
    ptr::null_mut()
}

fn force_dlopen_android_o_or_newer(path: &CStr, state: &LinkerState) -> *mut c_void {
    let dlopen: DlopenAndroidO = unsafe { mem::transmute(state.dlopen_symbol) };
    for caller in state.callers {
        if caller.is_null() {
            continue;
        }
        let handle = unsafe { dlopen(path.as_ptr(), libc::RTLD_NOW, caller) };
        if !handle.is_null() {
            return handle;
        }
    }
    ptr::null_mut()
}

struct PhdrScan {
    api_level: i32,
    linker: Option<LoadedElf>,
    callers: [*mut c_void; 3],
    vendor_match_limit: usize,
}

impl PhdrScan {
    fn new(api_level: i32) -> Self {
        Self {
            api_level,
            linker: None,
            callers: [ptr::null_mut(); 3],
            vendor_match_limit: VENDOR_CALLER_PREFIXES.len(),
        }
    }
}

#[derive(Clone, Copy)]
struct LoadedElf {
    base: usize,
    phdr: *const ElfPhdr,
    phnum: usize,
}

unsafe extern "C" fn scan_phdrs(
    info: *mut libc::dl_phdr_info,
    _size: usize,
    data: *mut c_void,
) -> c_int {
    if info.is_null() || data.is_null() {
        return 0;
    }
    let info = unsafe { &*info };
    if info.dlpi_name.is_null() || info.dlpi_phdr.is_null() {
        return 0;
    }
    let scan = unsafe { &mut *data.cast::<PhdrScan>() };
    let name = unsafe { CStr::from_ptr(info.dlpi_name) }.to_string_lossy();
    let loaded = LoadedElf {
        base: info.dlpi_addr as usize,
        phdr: info.dlpi_phdr,
        phnum: info.dlpi_phnum as usize,
    };

    if scan.linker.is_none() && is_linker_name(&name) {
        scan.linker = Some(loaded);
    }
    if scan.callers[0].is_null() && name.ends_with("/libc.so") {
        scan.callers[0] = first_load_address(loaded);
    }
    if scan.callers[1].is_null() && name.ends_with("/libart.so") {
        scan.callers[1] = first_load_address(loaded);
    }
    if scan.callers[2].is_null() && scan.vendor_match_limit > 0 {
        for (index, prefix) in VENDOR_CALLER_PREFIXES
            .iter()
            .take(scan.vendor_match_limit)
            .enumerate()
        {
            if name.starts_with(prefix) {
                scan.callers[2] = first_load_address(loaded);
                scan.vendor_match_limit = index;
                break;
            }
        }
    }

    if scan.linker.is_some()
        && !scan.callers[0].is_null()
        && !scan.callers[1].is_null()
        && (scan.api_level < API_O || !scan.callers[2].is_null() || scan.vendor_match_limit == 0)
    {
        1
    } else {
        0
    }
}

#[cfg(target_pointer_width = "64")]
const VENDOR_CALLER_PREFIXES: &[&str] = &[
    "/vendor/lib64/egl/",
    "/vendor/lib64/hw/",
    "/vendor/lib64/",
    "/odm/lib64/",
    "/vendor/lib64/vndk-sp/",
    "/odm/lib64/vndk-sp/",
];

#[cfg(target_pointer_width = "32")]
const VENDOR_CALLER_PREFIXES: &[&str] = &[
    "/vendor/lib/egl/",
    "/vendor/lib/hw/",
    "/vendor/lib/",
    "/odm/lib/",
    "/vendor/lib/vndk-sp/",
    "/odm/lib/vndk-sp/",
];

fn is_linker_name(name: &str) -> bool {
    name.ends_with("/linker")
        || name.ends_with("/linker64")
        || name == "linker"
        || name == "linker64"
}

fn first_load_address(elf: LoadedElf) -> *mut c_void {
    for index in 0..elf.phnum {
        let phdr = unsafe { &*elf.phdr.add(index) };
        if phdr.p_type == PT_LOAD {
            return (elf.base + phdr.p_vaddr as usize) as *mut c_void;
        }
    }
    ptr::null_mut()
}

fn find_loader_symbol(elf: LoadedElf, api_level: i32) -> Option<*mut c_void> {
    let table = DynamicSymbols::from_loaded_elf(elf)?;
    if (API_N..=API_N_MR1).contains(&api_level) {
        return table
            .find(LINKER_DLOPEN_EXT_N)
            .or_else(|| table.find(LINKER_DO_DLOPEN_N));
    }
    if api_level >= API_P {
        return table.find(LINKER_LOADER_DLOPEN_P);
    }
    table.find(LINKER_DLOPEN_O)
}

struct DynamicSymbols {
    strtab: *const c_char,
    symtab: *const ElfSym,
    symbol_count: usize,
}

impl DynamicSymbols {
    fn from_loaded_elf(elf: LoadedElf) -> Option<Self> {
        let dynamic = dynamic_segment(elf)?;
        let mut strtab = ptr::null();
        let mut symtab = ptr::null();
        let mut hash = ptr::null();
        let mut gnu_hash = ptr::null();

        let mut index = 0;
        loop {
            let entry = unsafe { &*dynamic.add(index) };
            match entry.d_tag as isize {
                DT_NULL => break,
                DT_STRTAB => strtab = entry.d_val as usize as *const c_char,
                DT_SYMTAB => symtab = entry.d_val as usize as *const ElfSym,
                DT_HASH => hash = entry.d_val as usize as *const u32,
                DT_GNU_HASH => gnu_hash = entry.d_val as usize as *const u32,
                _ => {}
            }
            index += 1;
        }

        if strtab.is_null() || symtab.is_null() {
            return None;
        }
        let symbol_count =
            symbol_count_from_hash(hash).or_else(|| symbol_count_from_gnu_hash(gnu_hash))?;
        if symbol_count == 0 {
            return None;
        }
        Some(Self {
            strtab,
            symtab,
            symbol_count,
        })
    }

    fn find(&self, name: &str) -> Option<*mut c_void> {
        for index in 0..self.symbol_count {
            let symbol = unsafe { &*self.symtab.add(index) };
            if symbol.st_name == 0 || symbol.st_value == 0 {
                continue;
            }
            let symbol_name = unsafe { CStr::from_ptr(self.strtab.add(symbol.st_name as usize)) };
            if symbol_name.to_bytes() == name.as_bytes() {
                return Some(symbol.st_value as usize as *mut c_void);
            }
        }
        None
    }
}

fn dynamic_segment(elf: LoadedElf) -> Option<*const ElfDyn> {
    for index in 0..elf.phnum {
        let phdr = unsafe { &*elf.phdr.add(index) };
        if phdr.p_type == PT_DYNAMIC {
            return Some((elf.base + phdr.p_vaddr as usize) as *const ElfDyn);
        }
    }
    None
}

fn symbol_count_from_hash(hash: *const u32) -> Option<usize> {
    if hash.is_null() {
        return None;
    }
    let nchain = unsafe { *hash.add(1) };
    Some(nchain as usize)
}

fn symbol_count_from_gnu_hash(gnu_hash: *const u32) -> Option<usize> {
    if gnu_hash.is_null() {
        return None;
    }

    let nbuckets = unsafe { *gnu_hash } as usize;
    let symoffset = unsafe { *gnu_hash.add(1) as usize };
    let bloom_size = unsafe { *gnu_hash.add(2) as usize };
    if nbuckets == 0 {
        return Some(symoffset);
    }

    let buckets = unsafe {
        gnu_hash
            .add(4)
            .cast::<ElfAddr>()
            .add(bloom_size)
            .cast::<u32>()
    };
    let chains = unsafe { buckets.add(nbuckets) };

    let mut max_symbol = 0_usize;
    for bucket_index in 0..nbuckets {
        let bucket = unsafe { *buckets.add(bucket_index) as usize };
        if bucket == 0 {
            continue;
        }
        let mut symbol = bucket;
        loop {
            max_symbol = max_symbol.max(symbol);
            let chain = unsafe { *chains.add(symbol - symoffset) };
            symbol += 1;
            if chain & 1 != 0 {
                break;
            }
        }
    }

    Some(max_symbol.saturating_add(1))
}

fn device_api_level() -> i32 {
    let mut value = [0 as c_char; 16];
    let len =
        unsafe { __system_property_get(c"ro.build.version.sdk".as_ptr(), value.as_mut_ptr()) };
    if len <= 0 {
        return 0;
    }
    let value = unsafe { CStr::from_ptr(value.as_ptr()) };
    value.to_string_lossy().parse::<i32>().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_linker_names() {
        assert!(is_linker_name("/apex/com.android.runtime/bin/linker64"));
        assert!(is_linker_name("/system/bin/linker"));
        assert!(is_linker_name("linker64"));
        assert!(!is_linker_name("/system/lib64/libdl.so"));
    }
}
