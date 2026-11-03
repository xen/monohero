use futures::channel::mpsc::UnboundedSender;
#[derive(Clone, Debug)]
pub enum UiEvent {
    Show,
    Quit,
}
#[cfg(target_os = "macos")]
pub fn create(sender: UnboundedSender<UiEvent>) -> Result<tray_icon::TrayIcon, String> {
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };
    let marker =
        objc2::MainThreadMarker::new().ok_or("Menu-bar icon must be created on the main thread")?;
    objc2_app_kit::NSApplication::sharedApplication(marker)
        .setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
    let menu = Menu::new();
    let open = MenuItem::new("Open queues", true, None);
    let quit = MenuItem::new("Quit dashboard", true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .map_err(|error| error.to_string())?;
    let open_id = open.id().clone();
    let quit_id = quit.id().clone();
    let clicks = sender.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = if event.id == open_id {
            Some(UiEvent::Show)
        } else if event.id == quit_id {
            Some(UiEvent::Quit)
        } else {
            None
        };
        if let Some(action) = action {
            let _ = clicks.unbounded_send(action);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if matches!(
            event,
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
        ) {
            let _ = sender.unbounded_send(UiEvent::Show);
        }
    }));
    let mut rgba = vec![0; 24 * 24 * 4];
    for y in [5, 11, 17] {
        for x in 4..20 {
            for dy in 0..2 {
                rgba[((y + dy) * 24 + x) * 4 + 3] = 255;
            }
        }
    }
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon_templated(Icon::from_rgba(rgba, 24, 24).map_err(|error| error.to_string())?)
        .with_tooltip("Monohero queues")
        .build()
        .map_err(|error| error.to_string())
}
