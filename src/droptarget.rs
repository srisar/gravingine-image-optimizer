//! OLE drop target for the main window, so the app knows when files are dragged *over* it
//! (to highlight the list), not only when they're dropped.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use winapi::shared::guiddef::{IsEqualIID, REFIID};
use winapi::shared::minwindef::{DWORD, ULONG};
use winapi::shared::windef::{HWND, POINTL};
use winapi::shared::winerror::{E_NOINTERFACE, HRESULT, S_OK};
use winapi::shared::wtypes::CLIPFORMAT;
use winapi::um::objidl::{FORMATETC, IDataObject, STGMEDIUM, TYMED_HGLOBAL};
use winapi::um::ole2::{OleInitialize, RegisterDragDrop, RevokeDragDrop};
use winapi::um::oleidl::{DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget, IDropTargetVtbl};
use winapi::um::shellapi::{DragQueryFileW, HDROP};
use winapi::um::unknwnbase::{IUnknown, IUnknownVtbl};
use winapi::um::winuser::CF_HDROP;
use winapi::Interface;

#[link(name = "ole32")]
unsafe extern "system" {
    fn ReleaseStgMedium(medium: *mut STGMEDIUM);
}

pub enum DragEvent {
    /// Files entered the window. Return `false` to refuse them (e.g. while busy).
    Enter,
    Leave,
    Drop(Vec<PathBuf>),
}

type Sink = Box<dyn Fn(DragEvent) -> bool>;

#[repr(C)]
struct DropTarget {
    vtbl: *const IDropTargetVtbl,
    refs: AtomicU32,
    accept: std::cell::Cell<bool>,
    sink: Sink,
}

static VTBL: IDropTargetVtbl = IDropTargetVtbl {
    parent: IUnknownVtbl { QueryInterface: query_interface, AddRef: add_ref, Release: release },
    DragEnter: drag_enter,
    DragOver: drag_over,
    DragLeave: drag_leave,
    Drop: drop_files,
};

/// Register `hwnd` as a drop target. The returned guard revokes it when dropped.
pub fn register(hwnd: HWND, sink: Sink) -> Option<Registration> {
    unsafe {
        let hr = OleInitialize(std::ptr::null_mut());
        if hr < 0 {
            return None;
        }
        let target = Box::into_raw(Box::new(DropTarget {
            vtbl: &VTBL,
            refs: AtomicU32::new(1),
            accept: std::cell::Cell::new(false),
            sink,
        }));
        if RegisterDragDrop(hwnd, target as *mut IDropTarget) < 0 {
            release(target as *mut IUnknown);
            return None;
        }
        // RegisterDragDrop holds its own reference; drop ours.
        release(target as *mut IUnknown);
        Some(Registration { hwnd })
    }
}

pub struct Registration {
    hwnd: HWND,
}

impl Drop for Registration {
    fn drop(&mut self) {
        unsafe { RevokeDragDrop(self.hwnd) };
    }
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP as CLIPFORMAT,
        ptd: std::ptr::null(),
        dwAspect: 1, // DVASPECT_CONTENT
        lindex: -1,
        tymed: TYMED_HGLOBAL,
    }
}

unsafe fn this<'a>(p: *mut IDropTarget) -> &'a DropTarget {
    unsafe { &*(p as *const DropTarget) }
}

unsafe extern "system" fn query_interface(this_: *mut IUnknown, riid: REFIID, out: *mut *mut winapi::ctypes::c_void) -> HRESULT {
    unsafe {
        if IsEqualIID(&*riid, &IUnknown::uuidof()) || IsEqualIID(&*riid, &IDropTarget::uuidof()) {
            add_ref(this_);
            *out = this_ as *mut _;
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn add_ref(this_: *mut IUnknown) -> ULONG {
    unsafe { (*(this_ as *const DropTarget)).refs.fetch_add(1, Ordering::SeqCst) + 1 }
}

unsafe extern "system" fn release(this_: *mut IUnknown) -> ULONG {
    unsafe {
        let left = (*(this_ as *const DropTarget)).refs.fetch_sub(1, Ordering::SeqCst) - 1;
        if left == 0 {
            drop(Box::from_raw(this_ as *mut DropTarget));
        }
        left
    }
}

// Note: Windows passes the drop point by value; winapi declares it as a pointer of the same
// size. It's never dereferenced here.
unsafe extern "system" fn drag_enter(
    this_: *mut IDropTarget,
    data: *const IDataObject,
    _keys: DWORD,
    _pt: *const POINTL,
    effect: *mut DWORD,
) -> HRESULT {
    unsafe {
        let t = this(this_);
        let has_files = (*data).QueryGetData(&hdrop_format()) == S_OK;
        let accept = has_files && (t.sink)(DragEvent::Enter);
        t.accept.set(accept);
        *effect = if accept { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        S_OK
    }
}

unsafe extern "system" fn drag_over(this_: *mut IDropTarget, _keys: DWORD, _pt: *const POINTL, effect: *mut DWORD) -> HRESULT {
    unsafe {
        *effect = if this(this_).accept.get() { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        S_OK
    }
}

unsafe extern "system" fn drag_leave(this_: *mut IDropTarget) -> HRESULT {
    unsafe {
        let t = this(this_);
        if t.accept.replace(false) {
            (t.sink)(DragEvent::Leave);
        }
        S_OK
    }
}

unsafe extern "system" fn drop_files(
    this_: *mut IDropTarget,
    data: *const IDataObject,
    _keys: DWORD,
    _pt: *const POINTL,
    effect: *mut DWORD,
) -> HRESULT {
    unsafe {
        let t = this(this_);
        if !t.accept.replace(false) {
            *effect = DROPEFFECT_NONE;
            return S_OK;
        }
        let mut medium: STGMEDIUM = std::mem::zeroed();
        let mut paths = Vec::new();
        if (*data).GetData(&hdrop_format(), &mut medium) == S_OK {
            let hdrop = *(*medium.u).hGlobal() as HDROP;
            let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
            }
            ReleaseStgMedium(&mut medium);
        }
        (t.sink)(DragEvent::Leave);
        if !paths.is_empty() {
            (t.sink)(DragEvent::Drop(paths));
        }
        *effect = DROPEFFECT_COPY;
        S_OK
    }
}
