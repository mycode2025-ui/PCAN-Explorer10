use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::{
    ComponentHandle,
    platform::{Platform, WindowAdapter, WindowEvent},
};
use std::rc::Rc;

slint::slint! {
    import { ProductModal, ProductDesign } from "../../../ui/design-system.slint";
    export component Probe inherits Window {
        width: 640px; height: 480px;
        in-out property <bool> opened: false;
        out property <bool> outside-focused: opener.has-focus;
        out property <bool> first-focused;
        out property <bool> last-focused;
        public function prepare() { opener.focus(); }
        opener := FocusScope { width: 20px; height: 20px; }
        ProductModal {
            open: root.opened;
            close-requested => { root.opened = false; }
            first := FocusScope { width: 20px; height: 20px; changed has-focus => { root.first-focused = self.has-focus; } }
            last := FocusScope { width: 20px; height: 20px; changed has-focus => { root.last-focused = self.has-focus; } }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

#[test]
fn modal_tabs_stay_inside_escape_restores_opener_and_reopens() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(Headless(window))).unwrap();
    let ui = Probe::new().unwrap();
    ui.show().unwrap();
    ui.invoke_prepare();
    assert!(ui.get_outside_focused());
    for _ in 0..2 {
        ui.set_opened(true);
        slint::platform::update_timers_and_animations();
        for _ in 0..6 {
            ui.window().dispatch_event(WindowEvent::KeyPressed {
                text: slint::platform::Key::Tab.into(),
            });
            ui.window().dispatch_event(WindowEvent::KeyReleased {
                text: slint::platform::Key::Tab.into(),
            });
            assert!(!ui.get_outside_focused(), "Tab escaped the modal");
            assert!(ui.get_first_focused() || ui.get_last_focused());
        }
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::Shift.into(),
        });
        for _ in 0..6 {
            ui.window().dispatch_event(WindowEvent::KeyPressed {
                text: slint::platform::Key::Tab.into(),
            });
            ui.window().dispatch_event(WindowEvent::KeyReleased {
                text: slint::platform::Key::Tab.into(),
            });
            assert!(!ui.get_outside_focused(), "Shift+Tab escaped the modal");
            assert!(ui.get_first_focused() || ui.get_last_focused());
        }
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: slint::platform::Key::Shift.into(),
        });
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: slint::platform::Key::Escape.into(),
        });
        slint::platform::update_timers_and_animations();
        assert!(!ui.get_opened(), "Escape did not close the modal");
        assert!(ui.get_outside_focused(), "Opener focus was not restored");
    }
}
