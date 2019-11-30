use std::cell::Cell;
use std::convert::TryInto;
use std::ptr::NonNull;
use std::{
    io,
    ptr,
};

use winapi::shared::minwindef::{
    HIWORD,
    LOWORD,
    LPARAM,
    LRESULT,
    UINT,
    WPARAM,
};
use winapi::shared::windef::HWND;
use winapi::um::shellapi::{
    NIN_KEYSELECT,
    NIN_SELECT,
};
use winapi::um::winuser::{
    DefWindowProcW,
    DispatchMessageW,
    GetMessageW,
    PostQuitMessage,
    TranslateMessage,
    MSG,
    WM_APP,
    WM_CONTEXTMENU,
    WM_DESTROY,
    WM_MENUCOMMAND,
    WM_QUIT,
};

use crate::internal::{
    catch_unwind_or_abort,
    ReturnValue,
};
use crate::ui::WindowHandle;

pub trait WindowMessageListener {
    #[allow(unused_variables)]
    #[inline(always)]
    fn handle_menu_command(
        &self,
        window: &WindowHandle,
        selected_item_idx: WPARAM,
        menu_handle: LPARAM,
    ) {
    }
    #[allow(unused_variables)]
    #[inline(always)]
    fn handle_window_destroy(&self, window: &WindowHandle) {}
    #[allow(unused_variables)]
    #[inline(always)]
    fn handle_notification_icon_select(&self, icon_id: u16) {}
    #[allow(unused_variables)]
    #[inline(always)]
    fn handle_notification_icon_context_select(&self, icon_id: u16) {}
    #[allow(unused_variables)]
    #[inline(always)]
    fn handle_custom_user_message(&self, window: &WindowHandle, message_id: u8) {}
}

#[derive(Copy, Clone)]
pub(crate) struct RawMessage {
    pub(crate) message: UINT,
    pub(crate) w_param: WPARAM,
    pub(crate) l_param: LPARAM,
}

impl RawMessage {
    /// Start of the message range for string message registered by `RegisterWindowMessage`.
    ///
    /// Values between `WM_APP` and this value (exclusive) can be used for private message IDs
    /// that won't conflict with messages from predefined Windows control classes.
    const STR_MSG_RANGE_START: u32 = 0xC000;

    pub(crate) const ID_NOTIFICATION_ICON_MSG: u32 = Self::STR_MSG_RANGE_START - 1;

    pub(crate) fn dispatch_to_message_listener<WML: WindowMessageListener>(
        self,
        window: WindowHandle,
        listener: &WML,
    ) -> Option<LRESULT> {
        let RawMessage {
            message,
            w_param,
            l_param,
        } = self;
        match message {
            value if value >= WM_APP && value <= WM_APP + (u8::max_value() as u32) => {
                listener
                    .handle_custom_user_message(&window, (message - WM_APP).try_into().unwrap());
                None
            }
            Self::ID_NOTIFICATION_ICON_MSG => {
                let icon_id = HIWORD(l_param as u32);
                let event_code = LOWORD(l_param as u32) as u32;
                match event_code {
                    // NIN_SELECT only happens with left clicks. Space will produce 1x NIN_KEYSELECT, Enter 2x NIN_KEYSELECT.
                    NIN_SELECT | NIN_KEYSELECT => listener.handle_notification_icon_select(icon_id),
                    // Works both with mouse right click and the context menu key.
                    WM_CONTEXTMENU => listener.handle_notification_icon_context_select(icon_id),
                    _ => (),
                }
                None
            }
            WM_MENUCOMMAND => {
                listener.handle_menu_command(&window, w_param, l_param);
                None
            }
            WM_DESTROY => {
                listener.handle_window_destroy(&window);
                None
            }
            _ => None,
        }
    }
}

thread_local! {
    static THREAD_LOOP_RUNNING: Cell<bool> = Cell::new(false);
}

pub enum ThreadMessageLoop {}

impl ThreadMessageLoop {
    pub fn run_thread_message_loop() -> io::Result<()> {
        THREAD_LOOP_RUNNING.with(|running| {
            if running.get() {
                panic!("Cannot run two thread message loops on the same thread");
            }
            running.set(true);
        });
        let mut msg: MSG = Default::default();
        loop {
            unsafe {
                GetMessageW(&mut msg, ptr::null_mut(), 0, 0)
                    .if_eq_to_error(-1, || io::Error::last_os_error())?;
            }
            if msg.message == WM_QUIT {
                THREAD_LOOP_RUNNING.with(|running| running.set(false));
                break;
            }
            unsafe {
                TranslateMessage(&mut msg);
                DispatchMessageW(&mut msg);
            }
        }
        Ok(())
    }

    pub fn post_quit_message() {
        if !ThreadMessageLoop::is_loop_running() {
            panic!("Cannot post quit message because thread message loop is not running");
        }
        unsafe {
            PostQuitMessage(0);
        }
    }

    #[inline(always)]
    fn is_loop_running() -> bool {
        THREAD_LOOP_RUNNING.with(|running| running.get())
    }
}

pub(crate) unsafe extern "system" fn generic_window_proc<WML>(
    h_wnd: HWND,
    message: UINT,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT
where
    WML: WindowMessageListener,
{
    let call = move || {
        let window = WindowHandle::from_non_null(
            NonNull::new(h_wnd)
                .expect("Window handle given to window procedure should never be NULL"),
        );

        let raw_message = RawMessage {
            message,
            w_param,
            l_param,
        };

        // When creating a window, the custom data for the loop is not set yet
        // before the first call to this function
        let listener_result = window
            .get_user_data_ptr::<WML>()
            .and_then(|mut listener_ptr| {
                raw_message.dispatch_to_message_listener(window, listener_ptr.as_ref())
            });

        if let Some(l_result) = listener_result {
            l_result
        } else {
            DefWindowProcW(h_wnd, message, w_param, l_param)
        }
    };
    catch_unwind_or_abort(call)
}
