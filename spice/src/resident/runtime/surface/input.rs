use std::future::Future;

#[derive(Clone, Copy)]
pub(super) enum Lead {
    Supervisor,
    Control,
    Record,
}

impl Lead {
    const fn next(self) -> Self {
        match self {
            Self::Supervisor => Self::Control,
            Self::Control => Self::Record,
            Self::Record => Self::Supervisor,
        }
    }
}

pub(super) enum Input<Supervisor, Control, Record> {
    Supervisor(Supervisor),
    Control(Control),
    Record(Record),
}

/// Select between the three live sources that can remain continuously ready.
///
/// Fixed priority strands admitted app commands while lanes keep publishing.
/// Rotating the lead bounds every ready source to two intervening turns.
pub(super) async fn next<Supervisor, Control, Record>(
    lead: &mut Lead,
    supervisor: impl Future<Output = Supervisor>,
    control: impl Future<Output = Control>,
    record: impl Future<Output = Record>,
) -> Input<Supervisor, Control, Record> {
    let leading = *lead;
    let selected = match leading {
        Lead::Supervisor => tokio::select! {
            biased;
            report = supervisor => Input::Supervisor(report),
            request = control => Input::Control(request),
            record = record => Input::Record(record),
        },
        Lead::Control => tokio::select! {
            biased;
            request = control => Input::Control(request),
            record = record => Input::Record(record),
            report = supervisor => Input::Supervisor(report),
        },
        Lead::Record => tokio::select! {
            biased;
            record = record => Input::Record(record),
            report = supervisor => Input::Supervisor(report),
            request = control => Input::Control(request),
        },
    };
    *lead = leading.next();
    selected
}

#[cfg(test)]
mod tests;
