//! Room-authority reads through the writer queue.

use std::sync::mpsc;

use super::{BusError, BusReadError, BusReader, FactPage, JournalRead, Request};

impl BusReader {
    /// Read one bounded page of current room-authority facts through a
    /// captured horizon.
    pub fn app_rooms(
        &self,
        start: journal::FactPageStart,
        horizon: journal::crossing::Seq,
        budget: journal::FactReadBudget,
    ) -> Result<FactPage, BusReadError> {
        let (answer, receiver) = mpsc::sync_channel(1);
        self.requests
            .send(Request::Read(JournalRead::AppRooms {
                start,
                horizon,
                budget,
                answer,
            }))
            .map_err(|_| BusReadError::Bus(BusError::Closed))?;
        receiver
            .recv()
            .map_err(|_| BusReadError::Bus(BusError::Closed))?
            .map_err(BusReadError::Head)
    }

    /// Read one room publication's admitted envelope, absent when
    /// `publication` names none in `room` through the current horizon.
    pub fn room_publication(
        &self,
        room: journal::crossing::TaskStamp,
        publication: journal::crossing::rooms::RoomAuthorityStamp,
    ) -> Result<Option<journal::crossing::Envelope<journal::crossing::AnyFact>>, BusReadError> {
        let (answer, receiver) = mpsc::sync_channel(1);
        self.requests
            .send(Request::Read(JournalRead::RoomPublication {
                room,
                publication,
                answer,
            }))
            .map_err(|_| BusReadError::Bus(BusError::Closed))?;
        receiver
            .recv()
            .map_err(|_| BusReadError::Bus(BusError::Closed))?
            .map_err(BusReadError::Head)
    }
}
