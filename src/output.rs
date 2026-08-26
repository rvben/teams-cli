use std::io::IsTerminal;

use clap::ValueEnum;
use serde::Serialize;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Auto,
    Text,
    Json,
}

#[derive(Debug, Clone, Copy)]
pub struct Output {
    pub format: OutputFormat,
    pub quiet: bool,
}

impl Output {
    pub fn json(self) -> bool {
        matches!(self.format, OutputFormat::Json)
            || (matches!(self.format, OutputFormat::Auto) && !std::io::stdout().is_terminal())
    }

    pub fn value<T: Serialize>(
        &self,
        value: &T,
        text: impl FnOnce() -> String,
    ) -> Result<(), AppError> {
        if self.json() {
            println!(
                "{}",
                serde_json::to_string_pretty(value)
                    .map_err(|e| AppError::Unexpected(e.to_string()))?
            );
        } else {
            println!("{}", text());
        }
        Ok(())
    }

    pub fn note(&self, message: impl AsRef<str>) {
        if !self.quiet {
            eprintln!("{}", message.as_ref());
        }
    }
}

pub fn print_error(error: &AppError, structured: bool) {
    if !structured {
        eprintln!("Error: {error}");
        eprintln!("Hint: run `teams doctor` for a guided diagnosis.");
    }
    let contract = error.contract();
    if structured {
        let envelope = serde_json::json!({
            "error": {
                "kind": contract.kind,
                "message": error.to_string(),
                "retryable": contract.retryable,
            }
        });
        eprintln!("{}", serde_json::to_string(&envelope).unwrap_or_else(|_| r#"{"error":{"kind":"unexpected_error","message":"error serialization failed","retryable":false}}"#.into()));
    }
}

pub fn structured_from_args() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let explicit_text = args.windows(2).any(|w| w == ["--output", "text"])
        || args.iter().any(|a| a == "--output=text" || a == "-otext");
    let explicit_json = args.windows(2).any(|w| w == ["--output", "json"])
        || args
            .iter()
            .any(|a| a == "--output=json" || a == "-ojson" || a == "--json");
    !explicit_text && (explicit_json || !std::io::stdout().is_terminal())
}
