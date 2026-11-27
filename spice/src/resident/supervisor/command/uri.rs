//! The `spice://` scheme a lane's omp reads and writes through: each request
//! answers with the [`crate::resource`] the URL names, over the lane's own
//! connection.

use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use super::Driver;
use crate::resource::{Access, Server};

/// The `spice://` server and the requests it is answering now. A withdrawn
/// request leaves the pending set, and only a request still in it is answered.
pub(super) struct Uris {
    server: Server,
    pending: Mutex<HashSet<String>>,
}

impl Uris {
    pub(super) fn new(server: Server) -> Self {
        Self {
            server,
            pending: Mutex::new(HashSet::new()),
        }
    }
}

fn append_result_frame(output: &mut Vec<u8>, id: &str, answer: &Result<String, String>) {
    match answer {
        Ok(content) => vendor::omp::append_host_uri_result(output, id, content),
        Err(reason) => vendor::omp::append_host_uri_refused(output, id, reason),
    }
}

fn result_frame(id: &str, answer: &Result<String, String>) -> Vec<u8> {
    let mut output = Vec::new();
    append_result_frame(&mut output, id, answer);
    output
}

fn write_notice(url: &str, answer: &Result<String, String>) -> String {
    let (kind, outcome) = match answer {
        Ok(receipt) => ("success receipt", receipt),
        Err(refusal) => ("refusal", refusal),
    };
    format!(
        "<system-notice>\nThe spice:// write to {url} returned this canonical {kind}:\n\n\
         {outcome}\n\nThis is a private write outcome; no acknowledgment key or visible ACK/NAK \
         is required.\n</system-notice>"
    )
}

impl<W: Write + Send + 'static> Driver<W> {
    /// Queue one exact background notice before the frame that releases its
    /// OMP boundary, writing and flushing both commands together.
    ///
    /// `after` is serialized only after the aside registration. An empty
    /// result still registers the notice, which lets a withdrawn durable
    /// operation disclose its outcome without answering the withdrawn call.
    fn background_before(
        &self,
        purpose: &str,
        message: &str,
        after: impl FnOnce(&mut Vec<u8>),
    ) -> io::Result<()> {
        let mut queued = self
            .background
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.activate()?;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let mut delivery = vendor::omp::aside_request(sequence, purpose, message);
        after(&mut delivery);
        self.write(&delivery)?;
        queued.insert(purpose.to_owned(), message.to_owned());
        Ok(())
    }

    /// Answer one host URI request, a read or a write carrying its content,
    /// on its own thread so the stream keeps being read. Any other operation
    /// is refused at once.
    ///
    /// Reading commits nothing, so omp may read as often as its tools ask.
    pub(super) fn serve_uri(
        self: &Arc<Self>,
        id: &str,
        operation: &str,
        url: &str,
        content: Option<&str>,
    ) {
        self.serve_uri_at(id, operation, url, content, None);
    }

    /// The captured-call path alone can carry authored-source provenance.
    pub(super) fn serve_uri_at(
        self: &Arc<Self>,
        id: &str,
        operation: &str,
        url: &str,
        content: Option<&str>,
        carrier: Option<wire::outbox::Carrier>,
    ) {
        let content = match (operation, content) {
            ("read", _) => None,
            ("write", Some(content)) => Some(content.to_owned()),
            _ => {
                let _refused =
                    self.reply(&vendor::omp::host_uri_refused(id, vendor::omp::URI_SERVED));
                return;
            }
        };
        self.uris
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned());
        let worker = self.worker();
        let (answered, url) = (id.to_owned(), url.to_owned());
        let started = std::thread::Builder::new()
            .name("spice-uri".to_owned())
            .spawn(move || {
                let access = match (content.as_deref(), carrier) {
                    (Some(text), Some(carrier)) => Access::Authored { text, carrier },
                    (Some(text), None) => Access::Write(text),
                    (None, Some(carrier)) => Access::CapturedRead(carrier),
                    (None, None) => Access::Read,
                };
                worker.driver.answer_uri(&answered, &url, access);
            });
        if let Err(error) = started {
            self.uris
                .pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(id);
            let _refused = self.reply(&vendor::omp::host_uri_refused(
                id,
                &format!("URI thread failed: {error}"),
            ));
        }
    }

    /// Finish one request after its resource operation. The pending set stays
    /// locked across delivery, so a withdrawal either suppresses the ordinary
    /// result or arrives after OMP already holds it.
    ///
    /// Reads have no durable outcome and stay silent once withdrawn. Writes
    /// always queue their native outcome for this exact occupant; the result,
    /// when still pending, follows that aside in the same stdin write.
    ///
    /// An admitted outbox receipt first withdraws the keys it acknowledged
    /// from queued reminders, so no reminder for them flushes with its notice.
    fn answer_uri(&self, id: &str, url: &str, access: Access<'_>) {
        let write = !matches!(&access, Access::Read | Access::CapturedRead(_));
        let authored = matches!(&access, Access::Authored { .. });
        let answer = (self.uris.server)(url, access);
        if authored
            && let Ok(rendered) = &answer
            && let Ok(wire::Outcome::Reply(reply)) =
                serde_json::from_str::<wire::Outcome<wire::outbox::Receipt>>(rendered)
        {
            self.cancel_receipted_reminders(&reply.body);
        }
        let mut pending = self
            .uris
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let pending = pending.remove(id);
        if !write {
            if pending {
                let _answered = self.reply(&result_frame(id, &answer));
            }
            return;
        }
        let purpose = format!("uri-write:{id}");
        let notice = write_notice(url, &answer);
        let noticed = self.background_before(&purpose, &notice, |delivery| {
            if pending {
                append_result_frame(delivery, id, &answer);
            }
        });
        // A parked driver cannot queue the notice, but the waiting call still
        // gets its result.
        if noticed.is_err() && pending {
            let _answered = self.reply(&result_frame(id, &answer));
        }
    }

    /// Silence the answer to a withdrawn request, if one is still pending.
    pub(super) fn withdraw_uri(&self, id: &str) {
        self.uris
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}

#[cfg(test)]
impl<W: Write + Send + 'static> Driver<W> {
    /// A driver whose `spice://` server answers reads through `read_uri` and
    /// refuses every write.
    pub(super) fn with_readers(
        stdin: W,
        read_inbox: impl Fn() -> Option<Vec<u8>> + Send + Sync + 'static,
        read_briefing: impl Fn() -> Option<String> + Send + Sync + 'static,
        read_uri: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
        seal: steer::Key,
        terminal: Option<super::Terminal>,
    ) -> Arc<Self> {
        let server: Server = Box::new(move |url, access| match access {
            Access::Read | Access::CapturedRead(_) => read_uri(url),
            Access::Write(_) | Access::Authored { .. } => Err(vendor::omp::URI_SERVED.to_owned()),
        });
        Self::with_server(
            stdin,
            super::Readers {
                inbox: Box::new(read_inbox),
                briefing: Box::new(read_briefing),
                ..super::Readers::default()
            },
            server,
            seal,
            terminal,
        )
    }

    /// A driver that reads pending steering and a resumed seat's notice when
    /// releasing its first prompt, and serves no `spice://` read.
    pub(super) fn with_startup_readers(
        stdin: W,
        read_inbox: impl Fn() -> Option<Vec<u8>> + Send + Sync + 'static,
        read_resume: impl Fn() -> String + Send + Sync + 'static,
    ) -> Arc<Self> {
        let server: Server = Box::new(|_, _| Err(vendor::omp::URI_SERVED.to_owned()));
        Self::with_server(
            stdin,
            super::Readers {
                inbox: Box::new(read_inbox),
                resume: Box::new(read_resume),
                ..super::Readers::default()
            },
            server,
            steer::Key::for_identity(crate::resident::new_uuid()),
            None,
        )
    }

    /// A driver that reads a resumed seat's notice through `read_resume` and
    /// serves no `spice://` read.
    pub(super) fn resuming(
        stdin: W,
        read_resume: impl Fn() -> String + Send + Sync + 'static,
    ) -> Arc<Self> {
        Self::seating(stdin, read_resume, || true)
    }

    /// A resuming driver whose occupant is seated whenever `read_seated` says.
    pub(super) fn seating(
        stdin: W,
        read_resume: impl Fn() -> String + Send + Sync + 'static,
        read_seated: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Arc<Self> {
        let server: Server = Box::new(|_, _| Err(vendor::omp::URI_SERVED.to_owned()));
        Self::with_server(
            stdin,
            super::Readers {
                resume: Box::new(read_resume),
                seated: Box::new(read_seated),
                ..super::Readers::default()
            },
            server,
            steer::Key::for_identity(crate::resident::new_uuid()),
            None,
        )
    }
}

#[cfg(test)]
mod tests;
