//! C: src/parser_util.c — utility functions for the config and commands
//! parser, ported 1:1. The stack is a single array of ten entries since the
//! number of values to store is very small.

/// C: include/parser_util.h — enum { STACK_STR = 0, STACK_LONG = 1 }
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StackType {
    /// C: STACK_STR = 0
    #[default]
    STACK_STR = 0,
    /// C: STACK_LONG = 1
    STACK_LONG = 1,
}

/// C: include/parser_util.h — union { char *str; long num; } val.
///
/// The C union is active depending on the entry type; the Rust port keeps
/// both members side by side (clearing the stack resets both, like the C
/// code does).
#[derive(Clone, Default, Debug)]
pub struct StackVal {
    /// C: char *str
    pub str: Option<String>,
    /// C: long num
    pub num: i64,
}

/// C: include/parser_util.h — struct stack_entry.
#[derive(Clone, Default, Debug)]
pub struct StackEntry {
    /// C: const char *identifier — a pointer, not dynamically allocated in
    /// C (it points into the token table); owned here.
    pub identifier: Option<String>,
    /// C: enum { STACK_STR, STACK_LONG } type
    pub r#type: StackType,
    /// C: union { char *str; long num; } val
    pub val: StackVal,
}

/// C: include/parser_util.h — struct stack.
#[derive(Clone, Default, Debug)]
pub struct Stack {
    /// C: struct stack_entry stack[10]
    pub stack: [StackEntry; 10],
}

/// C: src/parser_util.c:20 — pushes a string (identified by an identifier)
/// on the stack. If a string with the same identifier is already on the
/// stack, the new string is appended, separated by a comma.
pub fn parser_push_string(stack: &mut Stack, identifier: &str, str: &str) {
    for c in 0..10 {
        let entry = &mut stack.stack[c];
        if entry.identifier.as_deref().map_or(false, |id| id != identifier) {
            continue;
        }
        if entry.identifier.is_none() {
            // Found a free slot, store it here.
            stack.stack[c].identifier = Some(identifier.to_string());
            stack.stack[c].val.str = Some(str.to_string());
            stack.stack[c].r#type = StackType::STACK_STR;
        } else {
            // Append the value.
            // C: sasprintf(&stack->stack[c].val.str, "%s,%s", prev, str)
            let prev = entry.val.str.clone().unwrap_or_default();
            entry.val.str = Some(format!("{},{}", prev, str));
        }
        return;
    }

    // When we arrive here, the stack is full. This should not happen and
    // means there is either a bug in this parser or the specification
    // contains a command with more than 10 identified tokens.
    eprintln!(
        "BUG: parser stack full. This means either a bug in the code, or a new command which \
         contains more than 10 identified tokens."
    );
    std::process::exit(1);
}

/// C: src/parser_util.c:48 — pushes a long (identified by an identifier) on
/// the stack. Unlike parser_push_string this fills only free slots, so
/// pushing the same identifier twice occupies two slots (1:1 with the C
/// quirk).
pub fn parser_push_long(stack: &mut Stack, identifier: &str, num: i64) {
    for c in 0..10 {
        if stack.stack[c].identifier.is_some() {
            continue;
        }
        // Found a free slot, store it here.
        stack.stack[c].identifier = Some(identifier.to_string());
        stack.stack[c].val.num = num;
        stack.stack[c].r#type = StackType::STACK_LONG;
        return;
    }

    // When we arrive here, the stack is full. This should not happen and
    // means there is either a bug in this parser or the specification
    // contains a command with more than 10 identified tokens.
    eprintln!(
        "BUG: parser stack full. This means either a bug in the code, or a new command which \
         contains more than 10 identified tokens."
    );
    std::process::exit(1);
}

/// C: src/parser_util.c:77 — returns the string with the given identifier.
pub fn parser_get_string<'a>(stack: &'a Stack, identifier: &str) -> Option<&'a str> {
    for c in 0..10 {
        if stack.stack[c].identifier.is_none() {
            break;
        }
        if stack.stack[c].identifier.as_deref() == Some(identifier) {
            return stack.stack[c].val.str.as_deref();
        }
    }
    // C: return NULL
    None
}

/// C: src/parser_util.c:89 — returns the long with the given identifier.
pub fn parser_get_long(stack: &Stack, identifier: &str) -> i64 {
    for c in 0..10 {
        if stack.stack[c].identifier.is_none() {
            break;
        }
        if stack.stack[c].identifier.as_deref() == Some(identifier) {
            return stack.stack[c].val.num;
        }
    }
    // C: return 0
    0
}

/// C: src/parser_util.c:101 — clears the stack.
pub fn parser_clear_stack(stack: &mut Stack) {
    for c in 0..10 {
        stack.stack[c].identifier = None;
        stack.stack[c].val.str = None;
        stack.stack[c].val.num = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C: parser_util.c behaviour — push/get round trip.
    #[test]
    fn push_get_string() {
        let mut stack = Stack::default();
        parser_push_string(&mut stack, "workspace", "4");
        assert_eq!(parser_get_string(&stack, "workspace"), Some("4"));
        assert_eq!(parser_get_long(&stack, "workspace"), 0);
    }

    /// C: parser_push_string appends to the same identifier with a comma.
    #[test]
    fn push_string_appends() {
        let mut stack = Stack::default();
        parser_push_string(&mut stack, "command", "a");
        parser_push_string(&mut stack, "command", "b");
        assert_eq!(parser_get_string(&stack, "command"), Some("a,b"));
    }

    /// C: parser_push_long only fills free slots (no appending).
    #[test]
    fn push_long_twice_occupies_two_slots() {
        let mut stack = Stack::default();
        parser_push_long(&mut stack, "width", 10);
        parser_push_long(&mut stack, "width", 20);
        assert_eq!(parser_get_long(&stack, "width"), 10);
        assert!(stack.stack[1].identifier.is_some());
    }

    /// C: parser_clear_stack resets everything.
    #[test]
    fn clear_stack() {
        let mut stack = Stack::default();
        parser_push_string(&mut stack, "a", "x");
        parser_push_long(&mut stack, "b", 2);
        parser_clear_stack(&mut stack);
        assert_eq!(parser_get_string(&stack, "a"), None);
        assert_eq!(parser_get_long(&stack, "b"), 0);
        assert!(stack.stack[0].identifier.is_none());
    }

    /// C: get on an unknown identifier — NULL / 0.
    #[test]
    fn get_unknown() {
        let stack = Stack::default();
        assert_eq!(parser_get_string(&stack, "nope"), None);
        assert_eq!(parser_get_long(&stack, "nope"), 0);
    }
}
