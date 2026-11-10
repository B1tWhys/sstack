//! The bar across the top of the Session's pane, whose menu holds what the person can do to the Session
//! on screen besides talking to it: start another, rename it, archive it, and move it to another
//! Connection.

use iced::alignment::Vertical;
use iced::widget::{Column, Text, button, column, container, row, space, text, text_input};
use iced::{Element, Fill};
use lucide_icons::Icon;

use super::{Message, SPACING};
use crate::theme;

/// The id of the field the person renames the Session in, by which the window focuses it.
pub const RENAME: &str = "rename";

/// How wide each open menu is, in logical pixels.
const WIDTH: f32 = 240.0;

/// How wide the room for a Connection's check mark is, in logical pixels, so that the names line
/// up whether or not they have one.
const MARK_WIDTH: f32 = 18.0;

/// Which part of the menu is open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Menu {
    /// Nothing is open, and only the bar shows.
    #[default]
    Closed,
    /// The Session menu is open.
    Session,
    /// The Session menu is open with its list of Connections beside it.
    Connections,
    /// The Session menu is open, with a field in place of Rename holding the name typed so far.
    Renaming(String),
}

/// Something the person did to the menu itself, rather than through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEvent {
    /// Opens the Session menu, or closes the menu if anything is open.
    Toggle,
    /// Closes the menu, as a click outside it does.
    Close,
    /// Shows the list of Connections beside the Session menu, or hides it.
    Connections,
    /// Turns Rename into a field to type the new name in.
    Rename,
    /// The person changed the name in the field to this.
    RenameEdited(String),
}

impl Menu {
    /// Returns what is open after `event`. Renaming starts from `name`, what the Session is
    /// known by now.
    #[must_use]
    pub fn after(self, event: MenuEvent, name: impl FnOnce() -> String) -> Self {
        match (event, self) {
            (MenuEvent::Toggle, Self::Closed) | (MenuEvent::Connections, Self::Connections) => {
                Self::Session
            }
            (MenuEvent::Toggle | MenuEvent::Close, _) => Self::Closed,
            (MenuEvent::Connections, _) => Self::Connections,
            (MenuEvent::Rename, _) => Self::Renaming(name()),
            (MenuEvent::RenameEdited(edited), Self::Renaming(_)) => Self::Renaming(edited),
            (MenuEvent::RenameEdited(_), unchanged) => unchanged,
        }
    }
}

/// What the menu offers for the Session on screen.
pub struct Offer<'a> {
    /// Whether the Session can be archived now, which it cannot while a Turn runs.
    pub archivable: bool,
    /// The names of the Connections the Session can switch to. Empty before it is opened.
    pub connections: Vec<String>,
    /// The name of the Connection the Session runs on. Missing before it is opened.
    pub current: Option<&'a str>,
}

/// Draws the bar across the top of the Session's pane: the button that opens the Session menu,
/// drawn as pressed while `open`, and the Connection the Session runs on.
pub fn bar(current: Option<&str>, open: bool) -> Element<'_, Message> {
    let label = row![text("Session"), Text::from(Icon::ChevronDown)]
        .spacing(SPACING / 2.0)
        .align_y(Vertical::Center);
    let style = if open { button::primary } else { theme::quiet };
    let toggle = button(label)
        .style(style)
        .on_press(Message::Menu(MenuEvent::Toggle));
    let connection = current.map(|name| text(format!("Connection: {name}")));
    let items = row![toggle, space::horizontal(), connection].align_y(Vertical::Center);
    container(items)
        .style(theme::panel)
        .width(Fill)
        .padding([SPACING / 2.0, SPACING])
        .into()
}

/// Draws whatever part of the menu is open, to hang from the bar over the transcript. While the
/// menu is closed, it is empty space, which keeps its place in the window.
pub fn dropdown<'a>(menu: &'a Menu, offer: &Offer<'_>) -> Element<'a, Message> {
    let session = match menu {
        Menu::Closed => return space().into(),
        Menu::Session | Menu::Connections => session_menu(offer, None),
        Menu::Renaming(name) => session_menu(offer, Some(name)),
    };
    let connections = (*menu == Menu::Connections).then(|| connections(offer));
    row![session, connections].spacing(SPACING).into()
}

/// Draws the Session menu. With `renaming`, a field holding that name stands in for Rename.
fn session_menu<'a>(offer: &Offer<'_>, renaming: Option<&'a String>) -> Element<'a, Message> {
    let rename: Element<'a, Message> = match renaming {
        Some(name) => text_input("Session Name, or empty for the first prompt", name)
            .id(RENAME)
            .on_input(|edited| Message::Menu(MenuEvent::RenameEdited(edited)))
            .on_submit(Message::Renamed)
            .into(),
        None => item("Rename", Some(Message::Menu(MenuEvent::Rename))),
    };
    let archive = offer.archivable.then_some(Message::Archive);
    let items = column![
        item("New", Some(Message::NewSession)),
        item("New in main checkout", Some(Message::NewMainSession)),
        rename,
        item("Archive", archive),
        item("Connection >", Some(Message::Menu(MenuEvent::Connections))),
    ];
    panel(items)
}

/// Draws the list of Connections, with a check mark on the one the Session runs on.
fn connections(offer: &Offer<'_>) -> Element<'static, Message> {
    if offer.connections.is_empty() {
        return panel(column![text("No Connections").style(theme::muted)]);
    }
    let items = offer.connections.iter().map(|name| {
        let current = offer.current == Some(name.as_str());
        let mark = current.then(|| Text::from(Icon::Check));
        let label = row![container(mark).width(MARK_WIDTH), text(name.clone())];
        button(label)
            .style(button::text)
            .width(Fill)
            .on_press(Message::SwitchConnection(name.clone()))
            .into()
    });
    panel(Column::with_children(items))
}

/// Draws one thing the menu offers, which does nothing without a `message`.
fn item(label: &str, message: Option<Message>) -> Element<'_, Message> {
    button(label)
        .style(button::text)
        .width(Fill)
        .on_press_maybe(message)
        .into()
}

/// Draws `items` as one open menu.
fn panel(items: Column<'_, Message>) -> Element<'_, Message> {
    container(items)
        .style(theme::menu)
        .padding(SPACING / 2.0)
        .width(WIDTH)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns what is open after `events`, from closed, renaming from "named".
    fn after(events: impl IntoIterator<Item = MenuEvent>) -> Menu {
        events.into_iter().fold(Menu::Closed, |menu, event| {
            menu.after(event, || "named".into())
        })
    }

    #[test]
    fn the_menu_opens_and_closes_its_parts() {
        assert_eq!(after([MenuEvent::Toggle]), Menu::Session);
        assert_eq!(after([MenuEvent::Toggle, MenuEvent::Toggle]), Menu::Closed);
        assert_eq!(after([MenuEvent::Toggle, MenuEvent::Close]), Menu::Closed);
        let listed = [MenuEvent::Toggle, MenuEvent::Connections];
        assert_eq!(after(listed.clone()), Menu::Connections);
        let hidden = listed.into_iter().chain([MenuEvent::Connections]);
        assert_eq!(after(hidden), Menu::Session);
    }

    #[test]
    fn renaming_starts_from_the_name_and_follows_the_typing() {
        assert_eq!(after([MenuEvent::Rename]), Menu::Renaming("named".into()));
        let typed = [MenuEvent::Rename, MenuEvent::RenameEdited("new".into())];
        assert_eq!(after(typed), Menu::Renaming("new".into()));
        assert_eq!(
            after([MenuEvent::RenameEdited("lost".into())]),
            Menu::Closed
        );
    }
}
