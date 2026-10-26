use alloc::{format, string::String};
use core::{
    cell::RefCell,
    fmt::{self, Write},
};

use critical_section::Mutex;
use embassy_time::Instant;

pub const LAST_PRINT_TOPIC_LOG_MAX_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub enum TopicLogFullPolicy {
    StopAppending,
    DropOldestLines,
}

#[derive(Clone, Copy)]
pub struct TopicLogOptions {
    pub max_bytes: usize,
    pub preallocate_bytes: usize,
    pub full_policy: TopicLogFullPolicy,
}

struct TopicLogState {
    text: Option<String>,
    started_at: Option<Instant>,
    started_at_text: Option<String>,
    full: bool,
}

pub struct TopicLog {
    options: TopicLogOptions,
    state: Mutex<RefCell<TopicLogState>>,
}

impl TopicLog {
    pub const fn new(options: TopicLogOptions) -> Self {
        Self {
            options,
            state: Mutex::new(RefCell::new(TopicLogState {
                text: None,
                started_at: None,
                started_at_text: None,
                full: false,
            })),
        }
    }

    pub fn initialize(&self) {
        if self.options.preallocate_bytes == 0 {
            return;
        }

        let needs_buffer = critical_section::with(|cs| self.state.borrow_ref(cs).text.is_none());
        if !needs_buffer {
            return;
        }

        let buffer = String::with_capacity(self.options.preallocate_bytes.min(self.options.max_bytes));
        critical_section::with(|cs| {
            let mut state = self.state.borrow_ref_mut(cs);
            if state.text.is_none() {
                state.text = Some(buffer);
            }
        });
    }

    pub fn begin(&self, started_at_text: Option<String>) {
        self.initialize();
        critical_section::with(|cs| {
            let mut state = self.state.borrow_ref_mut(cs);
            state.text.get_or_insert_with(String::new).clear();
            state.started_at = Some(Instant::now());
            state.started_at_text = started_at_text;
            state.full = false;
        });
    }

    pub fn clear(&self) {
        self.begin(None);
    }

    pub fn append_fmt(&self, args: fmt::Arguments<'_>) {
        critical_section::with(|cs| {
            let mut state = self.state.borrow_ref_mut(cs);
            if state.full && matches!(self.options.full_policy, TopicLogFullPolicy::StopAppending) {
                return;
            }

            let started_at = state.started_at.get_or_insert_with(Instant::now);
            let elapsed_seconds = started_at.elapsed().as_secs();
            let line_length = formatted_length(format_args!("+{elapsed_seconds}s "))
                .saturating_add(formatted_length(args.clone()))
                .saturating_add(1);

            if line_length > self.options.max_bytes {
                state.full = true;
                return;
            }

            match self.options.full_policy {
                TopicLogFullPolicy::StopAppending => {
                    let text_length = state.text.as_ref().map_or(0, String::len);
                    if text_length.saturating_add(line_length) > self.options.max_bytes {
                        state.full = true;
                        return;
                    }
                }
                TopicLogFullPolicy::DropOldestLines => (),
            }

            let text = state.text.get_or_insert_with(String::new);
            if matches!(self.options.full_policy, TopicLogFullPolicy::DropOldestLines) {
                while text.len().saturating_add(line_length) > self.options.max_bytes {
                    let Some(line_end) = text.find('\n') else {
                        text.clear();
                        break;
                    };
                    text.drain(..=line_end);
                }
            }

            let _ = text.write_fmt(format_args!("+{elapsed_seconds}s "));
            let _ = text.write_fmt(args);
            text.push('\n');
        });
    }

    pub fn snapshot(&self) -> Option<String> {
        critical_section::with(|cs| {
            let state = self.state.borrow_ref(cs);
            let started_at = state.started_at?;
            let started_at_text = state.started_at_text.as_deref().unwrap_or("Time unavailable");
            let full_note = if state.full { "; full, later lines omitted" } else { "" };
            let text = state.text.as_deref().unwrap_or("");
            Some(format!(
                "Started: {started_at_text}; elapsed: {}s{full_note}\n{text}",
                started_at.elapsed().as_secs()
            ))
        })
    }
}

struct CountingWriter(usize);

impl Write for CountingWriter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        self.0 = self.0.saturating_add(value.len());
        Ok(())
    }
}

fn formatted_length(args: fmt::Arguments<'_>) -> usize {
    let mut writer = CountingWriter(0);
    let _ = fmt::write(&mut writer, args);
    writer.0
}

pub static LAST_PRINT_TOPIC_LOGS: TopicLog = TopicLog::new(TopicLogOptions {
    max_bytes: LAST_PRINT_TOPIC_LOG_MAX_BYTES,
    preallocate_bytes: LAST_PRINT_TOPIC_LOG_MAX_BYTES,
    full_policy: TopicLogFullPolicy::StopAppending,
});

pub fn last_print_info(args: fmt::Arguments<'_>) {
    framework::info!("{args}");
    LAST_PRINT_TOPIC_LOGS.append_fmt(args);
}

pub fn last_print_error(args: fmt::Arguments<'_>) {
    framework::error!("{args}");
    LAST_PRINT_TOPIC_LOGS.append_fmt(args);
}

#[macro_export]
macro_rules! last_print_topic_info {
    ($($arg:tt)+) => {{
        $crate::topic_logs::last_print_info(core::format_args!($($arg)+));
    }};
}

#[macro_export]
macro_rules! last_print_topic_error {
    ($($arg:tt)+) => {{
        $crate::topic_logs::last_print_error(core::format_args!($($arg)+));
    }};
}

#[macro_export]
macro_rules! last_print_topic_append {
    ($($arg:tt)+) => {{
        $crate::topic_logs::LAST_PRINT_TOPIC_LOGS.append_fmt(core::format_args!($($arg)+));
    }};
}
