//! Page-local singleton scheduling. Cancellation drops subscribers, not running JS.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

use gloo_timers::callback::Timeout;
use yew::{AttrValue, Callback};

use super::Theme;
use super::frame::{Frame, document_route};

const SOURCE_UNITS: usize = 50_000;
const QUEUE_JOBS: usize = 16;
const QUEUE_BYTES: usize = 1024 * 1024;
const SUBSCRIBERS: usize = 64;
const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const COALESCE_MILLIS: u32 = 80;
const RESPONSE_MILLIS: u32 = 30_000;

#[derive(Clone)]
pub(super) struct Render {
    pub(super) image: AttrValue,
    pub(super) geometry: Rc<crate::mermaid_geometry::Geometry>,
}

pub(super) type Outcome = Result<Render, AttrValue>;
pub(super) type Shared = Rc<RefCell<Service>>;

#[derive(Clone, PartialEq)]
pub(super) struct Key {
    pub(super) kind: super::RenderKind,
    pub(super) source: AttrValue,
    pub(super) theme: Theme,
    pub(super) generation: AttrValue,
    pub(super) node_ids: Rc<Vec<String>>,
}

pub(super) struct Subscriber {
    pub(super) id: u32,
    pub(super) host: web_sys::Element,
    pub(super) revision: AttrValue,
    pub(super) callback: Callback<Outcome>,
}

pub(super) struct RenderRequest {
    pub(super) kind: super::RenderKind,
    pub(super) source: AttrValue,
    pub(super) theme: Theme,
    pub(super) node_ids: Rc<Vec<String>>,
    pub(super) host: web_sys::Element,
    pub(super) revision: AttrValue,
    pub(super) callback: Callback<Outcome>,
}

impl Subscriber {
    fn attached(&self) -> bool {
        self.host.is_connected()
            && self.host.get_attribute("data-mermaid-revision").as_deref()
                == Some(self.revision.as_str())
    }

    fn deliver(self, outcome: Outcome) {
        if self.attached() {
            self.callback.emit(outcome);
        }
    }
}

pub(super) struct Job {
    pub(super) id: u32,
    pub(super) key: Key,
    pub(super) subscribers: Vec<Subscriber>,
    pub(super) decoding: Option<super::frame::DecodedImage>,
}

pub(super) struct Service {
    pub(super) generation: AttrValue,
    pub(super) channel: String,
    pub(super) frame: Option<Frame>,
    pub(super) active: Option<Job>,
    queue: VecDeque<Job>,
    cache: VecDeque<(Key, Render)>,
    cache_bytes: usize,
    serial: u32,
    dispatch: Option<Timeout>,
    deadline: Option<Timeout>,
}

pub(super) struct Lease {
    service: Weak<RefCell<Service>>,
    id: u32,
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(service) = self.service.upgrade() {
            service.borrow_mut().cancel(self.id);
        }
    }
}

fn cached_bytes(key: &Key, render: &Render) -> usize {
    key.source.len()
        + key.node_ids.iter().map(String::len).sum::<usize>()
        + render.image.len()
        + render
            .geometry
            .nodes
            .iter()
            .map(|node| std::mem::size_of_val(node) + node.id.len())
            .sum::<usize>()
}

impl Service {
    pub(super) fn new(generation: wire::Digest) -> Result<Shared, String> {
        let channel = super::super::random_uuid()?.to_string();
        Ok(Rc::new(RefCell::new(Self {
            generation: generation.to_string().into(),
            channel,
            frame: None,
            active: None,
            queue: VecDeque::new(),
            cache: VecDeque::new(),
            cache_bytes: 0,
            serial: 0,
            dispatch: None,
            deadline: None,
        })))
    }

    pub(super) fn request(shared: &Shared, request: RenderRequest) -> Result<Lease, AttrValue> {
        let RenderRequest {
            kind,
            source,
            theme,
            node_ids,
            host,
            revision,
            callback,
        } = request;
        if source.encode_utf16().take(SOURCE_UNITS + 1).count() > SOURCE_UNITS {
            return Err("Render source exceeds the 50,000 UTF-16 unit limit".into());
        }
        if kind == super::RenderKind::Diagram {
            crate::mermaid_geometry::validate_ids(&node_ids).map_err(AttrValue::from)?;
        } else if !node_ids.is_empty() {
            return Err("Math rendering does not accept diagram node IDs".into());
        }
        let mut service = shared.borrow_mut();
        let id = service
            .serial
            .checked_add(1)
            .ok_or("Renderer request limit reached; reload spice")?;
        service.serial = id;
        let key = Key {
            kind,
            source,
            theme,
            generation: service.generation.clone(),
            node_ids,
        };
        let subscriber = Subscriber {
            id,
            host,
            revision,
            callback,
        };
        // Rejection must not drop a lease while the mutable service borrow is held.
        let lease = || Lease {
            service: Rc::downgrade(shared),
            id,
        };
        if let Some(index) = service.cache.iter().position(|(cached, _)| *cached == key) {
            let entry = service.cache.remove(index).expect("located cache entry");
            let render = entry.1.clone();
            service.cache.push_back(entry);
            drop(service);
            subscriber.deliver(Ok(render));
            return Ok(lease());
        }
        service.prune();
        let subscribers = service
            .queue
            .iter()
            .chain(service.active.iter())
            .map(|job| job.subscribers.len())
            .sum::<usize>();
        if subscribers >= SUBSCRIBERS {
            return Err("Renderer has 64 pending subscribers; retry when it is idle".into());
        }
        if let Some(job) = service.active.as_mut().filter(|job| job.key == key) {
            job.subscribers.push(subscriber);
        } else if let Some(job) = service.queue.iter_mut().find(|job| job.key == key) {
            job.subscribers.push(subscriber);
        } else {
            let bytes = service
                .queue
                .iter()
                .map(|job| job.key.source.len())
                .sum::<usize>();
            if service.queue.len() >= QUEUE_JOBS || bytes + key.source.len() > QUEUE_BYTES {
                return Err("Render queue is full; retry when it is idle".into());
            }
            service.queue.push_back(Job {
                id,
                key,
                subscribers: vec![subscriber],
                decoding: None,
            });
        }
        drop(service);
        Self::schedule(shared);
        Ok(lease())
    }

    fn prune(&mut self) {
        for job in self.queue.iter_mut().chain(self.active.iter_mut()) {
            job.subscribers.retain(Subscriber::attached);
        }
        self.queue.retain(|job| !job.subscribers.is_empty());
    }

    fn cancel(&mut self, id: u32) {
        for job in self.queue.iter_mut().chain(self.active.iter_mut()) {
            job.subscribers.retain(|subscriber| subscriber.id != id);
        }
        self.queue.retain(|job| !job.subscribers.is_empty());
    }

    pub(super) fn schedule(shared: &Shared) {
        let mut service = shared.borrow_mut();
        service.prune();
        if service.queue.is_empty() || service.active.is_some() || service.dispatch.is_some() {
            return;
        }
        let weak = Rc::downgrade(shared);
        service.dispatch = Some(Timeout::new(COALESCE_MILLIS, move || {
            if let Some(shared) = weak.upgrade() {
                Self::pump(&shared);
            }
        }));
    }

    fn pump(shared: &Shared) {
        let mut service = shared.borrow_mut();
        service.dispatch = None;
        service.prune();
        if service.queue.is_empty() || service.active.is_some() {
            return;
        }
        if service.frame.is_none() {
            match Frame::open(shared, &service.generation) {
                Ok(frame) => {
                    service.frame = Some(frame);
                    service.arm_deadline(shared);
                    service.probe_retired();
                }
                Err(_) => {
                    drop(service);
                    Self::close(shared, "Could not load the isolated renderer");
                }
            }
            return;
        }
        if !service.frame.as_ref().is_some_and(|frame| frame.ready) {
            return;
        }
        service.active = service.queue.pop_front();
        let result = service.frame.as_ref().expect("loaded renderer").send(
            service.active.as_ref().expect("queued render"),
            &service.channel,
        );
        if result.is_err() {
            drop(service);
            Self::close(shared, "Could not send content to its renderer");
            return;
        }
        service.arm_deadline(shared);
    }

    fn arm_deadline(&mut self, shared: &Shared) {
        let weak = Rc::downgrade(shared);
        // This is a response deadline, not a CPU interrupt. An iframe can block
        // this event loop; recovery runs only once the browser can execute it.
        self.deadline = Some(Timeout::new(RESPONSE_MILLIS, move || {
            if let Some(shared) = weak.upgrade() {
                Self::close(&shared, "Renderer did not respond; retry or reload spice");
            }
        }));
    }

    pub(super) fn ready(shared: &Shared) {
        let mut service = shared.borrow_mut();
        if let Some(frame) = &mut service.frame {
            if frame.ready {
                return;
            }
            frame.ready = true;
            service.deadline = None;
        }
        drop(service);
        Self::schedule(shared);
    }

    pub(super) fn has_subscribers(job: &Job) -> bool {
        job.subscribers.iter().any(Subscriber::attached)
    }

    pub(super) fn finish(shared: &Shared, id: u32, outcome: Outcome) {
        let mut service = shared.borrow_mut();
        if !service.active.as_ref().is_some_and(|job| job.id == id) {
            return;
        }
        service.deadline = None;
        if outcome.is_err() {
            service.probe_retired();
        }
        let job = service.active.take().expect("matched active render");
        if Self::has_subscribers(&job)
            && let Ok(render) = &outcome
        {
            service.cache(job.key, render.clone());
        }
        drop(service);
        for subscriber in job.subscribers {
            subscriber.deliver(outcome.clone());
        }
        Self::schedule(shared);
    }

    fn cache(&mut self, key: Key, render: Render) {
        let bytes = cached_bytes(&key, &render);
        while self.cache.len() >= CACHE_ENTRIES || self.cache_bytes + bytes > CACHE_BYTES {
            let Some((old_key, old_render)) = self.cache.pop_front() else {
                return;
            };
            self.cache_bytes -= cached_bytes(&old_key, &old_render);
        }
        self.cache_bytes += bytes;
        self.cache.push_back((key, render));
    }

    pub(super) fn close(shared: &Shared, reason: &'static str) {
        let mut service = shared.borrow_mut();
        service.probe_retired();
        service.dispatch = None;
        service.deadline = None;
        service.frame = None;
        service.cache.clear();
        service.cache_bytes = 0;
        let active = service.active.take();
        let queue = std::mem::take(&mut service.queue);
        drop(service);
        let error = AttrValue::from(reason);
        for job in active.into_iter().chain(queue) {
            for subscriber in job.subscribers {
                subscriber.deliver(Err(error.clone()));
            }
        }
    }

    /// A renderer that cannot load may belong to a generation the resident no
    /// longer serves; that answer reloads the tab into the served one.
    fn probe_retired(&self) {
        wasm_bindgen_futures::spawn_local(super::super::connection::reload_if_retired(
            document_route(&self.generation),
        ));
    }
}
