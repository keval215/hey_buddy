//! Pure role mapping tables (AX role / UIA control type -> [`NodeRole`]). Linux-testable.
//! Unknown roles map to `Other`; the firewall then emits them only if they carry a label.

// Both tables are unit-tested everywhere; each OS uses only its own.
#![allow(dead_code)]
use crate::NodeRole;

/// macOS `AXRole` (+ optional `AXSubrole`) -> [`NodeRole`].
pub(crate) fn ax_role(role: &str, subrole: Option<&str>) -> NodeRole {
    match role {
        "AXWindow" => NodeRole::Window,
        "AXButton" => NodeRole::Button,
        "AXLink" => NodeRole::Link,
        "AXMenuItem" | "AXMenuBarItem" | "AXMenuButton" => NodeRole::MenuItem,
        // Tab buttons are radio buttons with a tab subrole.
        "AXRadioButton" if subrole == Some("AXTabButton") => NodeRole::Tab,
        "AXTab" => NodeRole::Tab,
        "AXCheckBox" => NodeRole::CheckBox,
        "AXRadioButton" => NodeRole::RadioButton,
        "AXComboBox" | "AXPopUpButton" => NodeRole::ComboBox,
        "AXTextField" | "AXTextArea" | "AXSearchField" | "AXSecureTextField" => NodeRole::TextField,
        "AXStaticText" => NodeRole::StaticText,
        "AXHeading" => NodeRole::Heading,
        "AXImage" => NodeRole::Image,
        "AXList" | "AXOutline" => NodeRole::List,
        "AXRow" | "AXCell" => NodeRole::ListItem,
        "AXTable" => NodeRole::Table,
        "AXToolbar" => NodeRole::Toolbar,
        "AXGroup" | "AXSplitGroup" | "AXScrollArea" | "AXLayoutArea" | "AXTabGroup" => {
            NodeRole::Group
        }
        "AXWebArea" => NodeRole::WebArea,
        _ => NodeRole::Other,
    }
}

/// Roles whose value is an editable text (value reduced to "has text").
pub(crate) fn ax_role_is_editable(role: &str) -> bool {
    matches!(
        role,
        "AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox" | "AXSecureTextField"
    )
}

/// Windows UIA `ControlType` id -> [`NodeRole`]. Document is the web page (title dropped).
pub(crate) fn uia_role(control_type: i32) -> NodeRole {
    match control_type {
        50000 | 50031 => NodeRole::Button, // Button, SplitButton
        50002 => NodeRole::CheckBox,
        50003 => NodeRole::ComboBox,
        50004 => NodeRole::TextField, // Edit
        50005 => NodeRole::Link,
        50006 => NodeRole::Image,
        50007 | 50024 | 50029 => NodeRole::ListItem, // ListItem, TreeItem, DataItem
        50008 | 50023 => NodeRole::List,             // List, Tree
        50011 => NodeRole::MenuItem,
        50013 => NodeRole::RadioButton,
        50018 | 50019 => NodeRole::Tab, // Tab, TabItem
        50020 => NodeRole::StaticText,
        50021 => NodeRole::Toolbar,
        50026 | 50033 | 50009 | 50010 => NodeRole::Group, // Group, Pane, Menu, MenuBar
        50028 | 50036 => NodeRole::Table,                 // DataGrid, Table
        50030 => NodeRole::WebArea,                       // Document
        50032 | 50037 => NodeRole::Window, // Window, TitleBar (its name is the window title)
        _ => NodeRole::Other,
    }
}

/// UIA control types holding editable text.
pub(crate) fn uia_is_editable(control_type: i32) -> bool {
    matches!(control_type, 50003 | 50004) // ComboBox, Edit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ax_table() {
        assert_eq!(ax_role("AXWindow", None), NodeRole::Window);
        assert_eq!(ax_role("AXButton", None), NodeRole::Button);
        assert_eq!(ax_role("AXRadioButton", None), NodeRole::RadioButton);
        assert_eq!(ax_role("AXRadioButton", Some("AXTabButton")), NodeRole::Tab);
        assert_eq!(ax_role("AXTextArea", None), NodeRole::TextField);
        assert_eq!(ax_role("AXSecureTextField", None), NodeRole::TextField);
        assert_eq!(ax_role("AXStaticText", None), NodeRole::StaticText);
        assert_eq!(ax_role("AXWebArea", None), NodeRole::WebArea);
        assert_eq!(ax_role("AXUnknown", None), NodeRole::Other);
        assert_eq!(ax_role("", None), NodeRole::Other);
    }

    #[test]
    fn ax_editable() {
        assert!(ax_role_is_editable("AXTextField"));
        assert!(ax_role_is_editable("AXComboBox"));
        assert!(!ax_role_is_editable("AXStaticText"));
        assert!(!ax_role_is_editable("AXButton"));
    }

    #[test]
    fn uia_table() {
        assert_eq!(uia_role(50000), NodeRole::Button);
        assert_eq!(uia_role(50004), NodeRole::TextField);
        assert_eq!(uia_role(50020), NodeRole::StaticText);
        assert_eq!(uia_role(50030), NodeRole::WebArea);
        assert_eq!(uia_role(50032), NodeRole::Window);
        assert_eq!(uia_role(50033), NodeRole::Group);
        assert_eq!(uia_role(50037), NodeRole::Window); // TitleBar
        assert_eq!(uia_role(50019), NodeRole::Tab);
        assert_eq!(ax_role("AXTab", None), NodeRole::Tab);
        assert_eq!(uia_role(0), NodeRole::Other);
        assert_eq!(uia_role(50025), NodeRole::Other);
        assert!(uia_is_editable(50004));
        assert!(uia_is_editable(50003));
        assert!(!uia_is_editable(50020));
    }
}
