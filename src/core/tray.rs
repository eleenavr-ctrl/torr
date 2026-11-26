use crate::core::diagnostics;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    ToggleWindow,
    ShowWindow,
    RestartServer,
    OpenWebPanel,
    ExitApp,
}

pub struct TrayHandle {
    thread_id: Arc<AtomicU32>,
    join_handle: Option<JoinHandle<()>>,
}

impl TrayHandle {
    pub fn spawn<F>(icon_rgba: Vec<u8>, on_command: F) -> Result<Self, String>
    where
        F: Fn(TrayCommand) + Send + Sync + 'static,
    {
        let thread_id = Arc::new(AtomicU32::new(0));
        let tid_clone = Arc::clone(&thread_id);
        let callback = Arc::new(on_command);

        let join_handle = thread::Builder::new()
            .name("tray-win32-loop".to_string())
            .spawn(move || {
                #[cfg(windows)]
                {
                    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
                    use windows_sys::Win32::UI::WindowsAndMessaging::{
                        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
                    };

                    let tid = unsafe { GetCurrentThreadId() };
                    tid_clone.store(tid, Ordering::SeqCst);

                    let menu = Menu::new();
                    let item_open = MenuItem::new("Открыть окно", true, None);
                    let item_restart = MenuItem::new("Перезапустить TorrServer", true, None);
                    let item_web = MenuItem::new("Открыть веб-панель в браузере", true, None);
                    let item_sep = PredefinedMenuItem::separator();
                    let item_exit = MenuItem::new("Выход", true, None);

                    let _ = menu.append(&item_open);
                    let _ = menu.append(&item_restart);
                    let _ = menu.append(&item_web);
                    let _ = menu.append(&item_sep);
                    let _ = menu.append(&item_exit);

                    let id_open = item_open.id().clone();
                    let id_restart = item_restart.id().clone();
                    let id_web = item_web.id().clone();
                    let id_exit = item_exit.id().clone();

                    let icon = match Icon::from_rgba(icon_rgba, 32, 32) {
                        Ok(ic) => ic,
                        Err(e) => {
                            diagnostics::error("[CORE]", format!("Ошибка создания иконки трея: {e}"));
                            return;
                        }
                    };

                    let _tray = match TrayIconBuilder::new()
                        .with_menu(Box::new(menu))
                        .with_tooltip("TorrServerUI")
                        .with_icon(icon)
                        .build()
                    {
                        Ok(t) => t,
                        Err(e) => {
                            diagnostics::error("[CORE]", format!("Ошибка инициализации трея: {e}"));
                            return;
                        }
                    };

                    let cb_tray = Arc::clone(&callback);
                    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                        match event {
                            TrayIconEvent::Click {
                                button: MouseButton::Left,
                                button_state: MouseButtonState::Up,
                                ..
                            }
                            | TrayIconEvent::DoubleClick {
                                button: MouseButton::Left,
                                ..
                            } => {
                                cb_tray(TrayCommand::ToggleWindow);
                            }
                            _ => {}
                        }
                    }));

                    let cb_menu = Arc::clone(&callback);
                    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                        if event.id == id_open {
                            cb_menu(TrayCommand::ShowWindow);
                        } else if event.id == id_restart {
                            cb_menu(TrayCommand::RestartServer);
                        } else if event.id == id_web {
                            cb_menu(TrayCommand::OpenWebPanel);
                        } else if event.id == id_exit {
                            cb_menu(TrayCommand::ExitApp);
                        }
                    }));

                    diagnostics::info("[CORE]", "Системный трей инициализирован");

                    unsafe {
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }

                #[cfg(not(windows))]
                {
                    let _ = (icon_rgba, tid_clone, callback);
                }
            })
            .map_err(|e| format!("Не удалось запустить поток трея: {e}"))?;

        Ok(Self {
            thread_id,
            join_handle: Some(join_handle),
        })
    }

    pub fn shutdown(&mut self) {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_QUIT};
            let tid = self.thread_id.load(Ordering::SeqCst);
            if tid != 0 {
                unsafe {
                    PostThreadMessageW(tid, WM_QUIT, 0, 0);
                }
            }
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}
