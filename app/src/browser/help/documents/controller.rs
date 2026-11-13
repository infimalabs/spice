use std::{cell::RefCell, rc::Rc};

use wire::intent::{RackAnswer, RackContent, RackRead};
use yew::prelude::*;

use crate::client_action::ClientAction;
use crate::documentation::{
    Documents, Link, Read, Target, Visit, is_markdown_content_type, resolve,
};

#[derive(Clone)]
pub(super) struct Controller {
    pub(super) state: Rc<RefCell<Documents>>,
    pub(super) redraw: UseForceUpdateHandle,
    pub(super) on_action: Callback<ClientAction, bool>,
    pub(super) online: bool,
}

impl Controller {
    pub(super) fn load(&self, target: Target, visit: Visit) {
        let read = self.state.borrow_mut().begin(target, visit);
        self.redraw.force_update();
        if !self.online {
            self.state.borrow_mut().fail(
                &read,
                "Documentation is unavailable while disconnected.".into(),
            );
            return;
        }
        let controller = self.clone();
        let request = RackRead {
            path: read.target.path.clone(),
            generation: None,
        };
        let respond = Rc::new(move |answer| {
            let accepted = controller
                .state
                .borrow_mut()
                .accept(&read, validate(answer));
            if accepted {
                controller.load_images(&read);
            }
            controller.redraw.force_update();
        });
        self.send(request, respond);
    }

    pub(super) fn retry(&self) {
        let (target, visit) = {
            let state = self.state.borrow();
            state.failed.as_ref().map_or_else(
                || {
                    (
                        state.target(),
                        if state.history.is_empty() {
                            Visit::Push
                        } else {
                            Visit::Reload
                        },
                    )
                },
                |read| (read.target.clone(), read.visit.clone()),
            )
        };
        self.load(target, visit);
    }

    fn send(&self, request: RackRead, respond: crate::client_action::RackResponse) {
        if !self.on_action.emit(ClientAction::Rack {
            request,
            respond: respond.clone(),
        }) {
            respond(Err(
                "Documentation read could not be queued; retry when connected.".into(),
            ));
        }
    }

    fn load_images(&self, read: &Read) {
        let Some(page) = self.state.borrow().page.clone() else {
            return;
        };
        let RackContent::Text { text, content_type } = &page.content else {
            return;
        };
        if !is_markdown_content_type(content_type) {
            return;
        }
        let mut paths = Vec::new();
        for source in crate::composer_markdown::document::image_sources(text) {
            let Link::Internal(target) = resolve(&page.path, false, &source) else {
                continue;
            };
            if self
                .state
                .borrow()
                .cached(&page.identity, &target.path)
                .is_none()
                && !paths.contains(&target.path)
            {
                paths.push(target.path);
            }
        }
        self.state.borrow_mut().media_pending = paths.len();
        for path in paths {
            let controller = self.clone();
            let identity = page.identity.clone();
            let read = read.clone();
            let request = RackRead {
                path: path.clone(),
                generation: identity.generation,
            };
            self.send(
                request,
                Rc::new(move |answer| {
                    let mut state = controller.state.borrow_mut();
                    if !state.current(&read) {
                        return;
                    }
                    state.media_pending = state.media_pending.saturating_sub(1);
                    match validate(answer) {
                        Ok(answer) if matches!(answer.content, RackContent::Image { .. }) => {
                            state.cache_media(&read, &identity, &path, answer);
                        }
                        Ok(_) => state.fail(
                            &read,
                            format!("Documentation image is not a raster image: {path}"),
                        ),
                        Err(issue) => state.fail(&read, issue),
                    }
                    let settled = state.media_pending == 0;
                    drop(state);
                    if settled {
                        controller.redraw.force_update();
                    }
                }),
            );
        }
    }
}

fn validate(answer: Result<RackAnswer, String>) -> Result<RackAnswer, String> {
    let answer = answer?;
    if let RackContent::Image { media } = &answer.content {
        media.validate().map_err(|error| error.to_string())?;
        if !matches!(
            media.content_type.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif"
        ) {
            return Err("Documentation images must be admitted raster media.".into());
        }
    }
    Ok(answer)
}
