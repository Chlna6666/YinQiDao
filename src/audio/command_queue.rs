use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const MAX_PENDING_COMMANDS: usize = 64;

pub(crate) struct CommandQueue<T> {
    commands: Mutex<VecDeque<T>>,
    closed: AtomicBool,
}

impl<T> CommandQueue<T> {
    pub(crate) fn new() -> Self {
        Self {
            commands: Mutex::new(VecDeque::with_capacity(MAX_PENDING_COMMANDS)),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) fn push(&self, command: T, can_replace: impl Fn(&T, &T) -> bool) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let Ok(mut commands) = self.commands.lock() else {
            return false;
        };
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        if let Some(index) = commands
            .iter()
            .rposition(|queued| can_replace(queued, &command))
        {
            // Replace in place so a queued transport command keeps its ordering relative to
            // configuration commands that arrived before or after it.
            commands[index] = command;
        } else if commands.len() >= MAX_PENDING_COMMANDS {
            return false;
        } else {
            commands.push_back(command);
        }
        true
    }

    pub(crate) fn pop(&self) -> Option<T> {
        self.commands.lock().ok()?.pop_front()
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_replaceable_command_does_not_grow_queue() {
        let queue = CommandQueue::new();
        for value in 0..256 {
            assert!(queue.push(value, |queued, incoming| { queued % 2 == incoming % 2 }));
        }
        assert_eq!(queue.pop(), Some(254));
        assert_eq!(queue.pop(), Some(255));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn full_queue_rejects_non_replaceable_command() {
        let queue = CommandQueue::new();
        for value in 0..MAX_PENDING_COMMANDS {
            assert!(queue.push(value, |queued, incoming| queued == incoming));
        }
        assert!(!queue.push(MAX_PENDING_COMMANDS, |queued, incoming| {
            queued == incoming
        }));
    }
}
