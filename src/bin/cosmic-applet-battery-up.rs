use battery_up::{read_battery_state, BatteryState};
use cosmic::{
    app, applet,
    applet::cosmic_panel_config::PanelAnchor,
    iced::{
        time,
        widget::{container, row, space},
        Alignment, Length, Subscription,
    },
    theme,
    widget::{button, icon, text},
    Element,
};
use std::path::PathBuf;
use std::time::Duration;

const APP_ID: &str = "dev.lluz.BatteryUpApplet";
const DEFAULT_STATE_FILE: &str = "/var/lib/battery-up/state";

fn main() -> cosmic::iced::Result {
    applet::run::<BatteryUpApplet>(())
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
}

#[derive(Default)]
struct BatteryUpApplet {
    core: app::Core,
    state_file: PathBuf,
    state: Option<BatteryState>,
    error: Option<String>,
}

impl BatteryUpApplet {
    fn refresh(&mut self) {
        match read_battery_state(&self.state_file) {
            Ok(state) => {
                self.state = Some(state);
                self.error = None;
            }
            Err(err) => {
                self.state = None;
                self.error = Some(err.to_string());
            }
        }
    }

    fn label(&self) -> String {
        self.state
            .as_ref()
            .map(|state| format_duration(state.counted_seconds))
            .unwrap_or_else(|| "--:--:--".to_string())
    }

    fn icon_name(&self) -> &'static str {
        match self.state.as_ref() {
            Some(state) if state.on_battery_only => "battery-good-symbolic",
            Some(_) => "battery-full-charging-symbolic",
            None => "battery-missing-symbolic",
        }
    }
}

impl cosmic::Application for BatteryUpApplet {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn init(core: app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let state_file = std::env::var_os("BATTERY_UP_STATE_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_FILE));

        let mut applet = Self {
            core,
            state_file,
            state: None,
            error: None,
        };
        applet.refresh();

        (applet, app::Task::none())
    }

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(applet::style())
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        time::every(Duration::from_secs(5)).map(|_| Message::Tick)
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::Tick => self.refresh(),
        }

        app::Task::none()
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );
        let label = self.label();
        let icon = icon::from_name(self.icon_name()).size(16);

        let content: Element<'_, Message> = if horizontal {
            row![
                icon,
                text::caption(label),
                container(space::vertical().height(Length::Fixed(
                    (self.core.applet.suggested_size(true).1
                        + 2 * self.core.applet.suggested_padding(true).1)
                        as f32
                )))
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![icon]
                .align_y(Alignment::Center)
                .height(Length::Fill)
                .into()
        };

        button::custom(content)
            .padding(if horizontal {
                [0, self.core.applet.suggested_padding(true).0]
            } else {
                [self.core.applet.suggested_padding(true).0, 0]
            })
            .on_press(Message::Tick)
            .class(theme::Button::AppletIcon)
            .into()
    }
}

fn format_duration(total: u64) -> String {
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}
