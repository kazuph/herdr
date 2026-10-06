//! Editing semantics shared by the existing rename dialogs.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(crate) fn clear(input: &mut String, replace_on_type: &mut bool) {
    input.clear();
    *replace_on_type = false;
}

pub(crate) fn insert(input: &mut String, replace_on_type: &mut bool, text: &str) {
    if *replace_on_type {
        clear(input, replace_on_type);
    }
    input.push_str(text);
}

fn delete_char(input: &mut String, replace_on_type: &mut bool) {
    if *replace_on_type {
        clear(input, replace_on_type);
    } else {
        input.pop();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenameWordDeleteClass {
    Word,
    Separator,
}

fn rename_word_delete_class(ch: char) -> RenameWordDeleteClass {
    if ch.is_alphanumeric() || ch == '_' {
        RenameWordDeleteClass::Word
    } else {
        RenameWordDeleteClass::Separator
    }
}

fn delete_word(input: &mut String, replace_on_type: &mut bool) {
    if *replace_on_type {
        clear(input, replace_on_type);
        return;
    }

    while input.chars().last().is_some_and(char::is_whitespace) {
        input.pop();
    }

    let Some(class) = input.chars().last().map(rename_word_delete_class) else {
        return;
    };

    while input
        .chars()
        .last()
        .is_some_and(|ch| !ch.is_whitespace() && rename_word_delete_class(ch) == class)
    {
        input.pop();
    }
}

pub(crate) fn edit_key(input: &mut String, replace_on_type: &mut bool, key: KeyEvent) {
    match key.code {
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            clear(input, replace_on_type);
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => {
            clear(input, replace_on_type);
        }
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT) =>
        {
            delete_word(input, replace_on_type);
        }
        KeyCode::Char('h' | 'w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            delete_word(input, replace_on_type);
        }
        KeyCode::Backspace => delete_char(input, replace_on_type),
        KeyCode::Char(c) if key.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
            insert(input, replace_on_type, &c.to_string());
        }
        _ => {}
    }
}
