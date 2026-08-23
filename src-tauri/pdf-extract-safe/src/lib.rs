use std::{
    cell::Cell,
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
};

thread_local! {
    static PDF_EXTRACTION_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct ExtractionGuard;

impl ExtractionGuard {
    fn enter() -> Self {
        PDF_EXTRACTION_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self
    }
}

impl Drop for ExtractionGuard {
    fn drop(&mut self) {
        PDF_EXTRACTION_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Parser(String),
    Panicked(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parser(message) => write!(f, "pdf parser error: {message}"),
            Self::Panicked(message) => write!(f, "pdf parser panicked: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub fn is_recovering_panic() -> bool {
    PDF_EXTRACTION_DEPTH.with(|depth| depth.get() > 0)
}

pub fn extract_text_from_mem(bytes: &[u8]) -> Result<String, Error> {
    let _guard = ExtractionGuard::enter();
    match catch_unwind(AssertUnwindSafe(|| {
        pdf_extract_upstream::extract_text_from_mem(bytes)
    })) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(error)) => Err(Error::Parser(format!("{error:?}"))),
        Err(payload) => Err(Error::Panicked(panic_payload(&payload))),
    }
}

fn panic_payload(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).replace('"', "'")
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.replace('"', "'")
    } else {
        "non-string panic payload".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_pdf_returns_error_without_unwinding() {
        let result = extract_text_from_mem(b"not a pdf");
        assert!(result.is_err());
        assert!(!is_recovering_panic());
    }

    #[test]
    fn recovery_flag_is_reset_after_caught_panic() {
        let _guard = ExtractionGuard::enter();
        assert!(is_recovering_panic());
        drop(_guard);
        assert!(!is_recovering_panic());
    }
}
