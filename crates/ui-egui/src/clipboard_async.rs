//! Clipboard platforms whose permission prompt and read must leave the UI thread free.
use std::sync::mpsc::{Receiver, TryRecvError};

use serde_json::{Value, json};

use crate::PhotocraftApp;

pub type Image = (u32, u32, Vec<u8>);
pub type Read = Box<dyn FnMut() -> Receiver<Result<Option<Image>, String>>>;

pub(crate) struct Pending {
    receiver: Receiver<Result<Option<Image>, String>>,
    command: String,
    params: Value,
    document: Option<photocraft_doc::DocId>,
    automation: bool,
}

pub(crate) fn begin(app: &mut PhotocraftApp, command: &str, params: &Value) -> Option<Result<Value, String>> {
    let read = app.services.clipboard_read_async.as_mut()?;
    if app.pending_clipboard.is_some() {
        return Some(Err("a clipboard read is already in progress".into()));
    }
    app.pending_clipboard = Some(Pending {
        receiver: read(),
        command: command.into(),
        params: params.clone(),
        document: app.session.active().map(|state| state.doc.id),
        automation: app.automation_input,
    });
    Some(Ok(json!({"clipboard": "pending"})))
}

pub(crate) fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(pending) = app.pending_clipboard.take() else { return };
    let result = match pending.receiver.try_recv() {
        Err(TryRecvError::Empty) => {
            app.pending_clipboard = Some(pending);
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
            return;
        }
        Err(TryRecvError::Disconnected) => Err("clipboard reader stopped without an answer".into()),
        Ok(result) => result,
    };
    if pending.document != app.session.active().map(|state| state.doc.id) {
        app.ui.status = "Paste cancelled because the active document changed".into();
        return;
    }
    let result = result.and_then(|image| {
        if let Some(image) = image {
            app.import_clipboard_image(image);
        }
        if app.session.clipboard.is_none() {
            return Err("Nothing to paste: the clipboard holds no image".into());
        }
        // Resume the same command with the authority it had when requested.
        let params = if pending.command == "edit.paste" && pending.params.as_object().is_none_or(|o| o.is_empty()) {
            app.default_paste_params()
        } else {
            pending.params
        };
        let service = app.services.clipboard_read_async.take();
        let automation = std::mem::replace(&mut app.automation_input, pending.automation);
        app.clip_read_for_paste = true;
        let result = app.run(&pending.command, params);
        app.automation_input = automation;
        app.services.clipboard_read_async = service;
        result.map(|_| ())
    });
    if let Err(error) = result {
        app.ui.status = error;
        app.ui.status_error = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use photocraft_engine::Session;
    use std::sync::mpsc;

    fn app() -> (PhotocraftApp, mpsc::Sender<Result<Option<Image>, String>>) {
        let (tx, rx) = mpsc::channel();
        let mut receiver = Some(rx);
        let services = Services { clipboard_read_async: Some(Box::new(move || receiver.take().unwrap())), ..Default::default() };
        (PhotocraftApp::new(Session::new(), services), tx)
    }

    #[test]
    fn new_document_waits_for_the_system_clipboard() {
        let (mut app, tx) = app();
        let result = app.run("file.newFromClipboard", json!({})).unwrap();
        assert_eq!(result["clipboard"], "pending");
        assert!(app.session.active().is_none());
        poll(&mut app, &egui::Context::default());
        assert!(app.session.active().is_none());
        tx.send(Ok(Some((2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255])))).unwrap();
        poll(&mut app, &egui::Context::default());
        let doc = &app.session.active().unwrap().doc;
        assert_eq!((doc.size.width, doc.size.height), (2, 1));
        assert!(app.pending_clipboard.is_none());
    }

    #[test]
    fn delayed_paste_cannot_modify_a_different_document() {
        let (mut app, tx) = app();
        app.run("file.newFromClipboard", json!({})).unwrap();
        app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
        let before = app.session.active().unwrap().doc.layers.len();
        tx.send(Ok(Some((1, 1, vec![255; 4])))).unwrap();
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), before);
        assert!(app.ui.status.contains("active document changed"));
        assert!(app.session.clipboard.is_none());
    }

    #[test]
    fn permission_failure_is_reported_and_clears_the_pending_request() {
        let (mut app, tx) = app();
        app.run("file.newFromClipboard", json!({})).unwrap();
        tx.send(Err("permission denied".into())).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(app.ui.status_error);
        assert_eq!(app.ui.status, "permission denied");
        assert!(app.pending_clipboard.is_none());
    }
}
