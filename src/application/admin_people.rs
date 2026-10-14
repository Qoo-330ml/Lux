use crate::{
    application::people::PeopleService, domain::ids::LibraryId,
    storage::StoredPersonIndexRebuildJob,
};

pub(crate) struct PersonIndexRebuildPage {
    pub(crate) jobs: Vec<StoredPersonIndexRebuildJob>,
    pub(crate) total: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonIndexRebuildRequestError {
    AlreadyRunning,
    LibraryNotFound,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersonIndexRebuildCancelError {
    NotCancellable,
    Unavailable,
}

pub(crate) async fn list_person_index_rebuild_jobs(
    people: &PeopleService,
    offset: i64,
    limit: i64,
) -> Result<PersonIndexRebuildPage, ()> {
    let total = people
        .count_person_index_rebuild_jobs()
        .await
        .map_err(|_| ())?;
    let jobs = people
        .list_person_index_rebuild_jobs(offset, limit)
        .await
        .map_err(|_| ())?;
    Ok(PersonIndexRebuildPage { jobs, total })
}

pub(crate) async fn queue_person_index_rebuild(
    people: &PeopleService,
    library_id: LibraryId,
) -> Result<StoredPersonIndexRebuildJob, PersonIndexRebuildRequestError> {
    let library_id = library_id.to_string();
    match people.queue_person_index_rebuild(&library_id).await {
        Ok(true) => {}
        Ok(false) => {
            return match people.get_person_index_rebuild_job(&library_id).await {
                Ok(Some(job)) if job.status == "RUNNING" => {
                    Err(PersonIndexRebuildRequestError::AlreadyRunning)
                }
                Ok(_) | Err(_) => Err(PersonIndexRebuildRequestError::LibraryNotFound),
            };
        }
        Err(_) => return Err(PersonIndexRebuildRequestError::Unavailable),
    }
    people
        .get_person_index_rebuild_job(&library_id)
        .await
        .map_err(|_| PersonIndexRebuildRequestError::Unavailable)?
        .ok_or(PersonIndexRebuildRequestError::Unavailable)
}

pub(crate) async fn cancel_person_index_rebuild(
    people: &PeopleService,
    library_id: LibraryId,
) -> Result<StoredPersonIndexRebuildJob, PersonIndexRebuildCancelError> {
    let library_id = library_id.to_string();
    match people.cancel_person_index_rebuild(&library_id).await {
        Ok(true) => {}
        Ok(false) => return Err(PersonIndexRebuildCancelError::NotCancellable),
        Err(_) => return Err(PersonIndexRebuildCancelError::Unavailable),
    }
    people
        .get_person_index_rebuild_job(&library_id)
        .await
        .map_err(|_| PersonIndexRebuildCancelError::Unavailable)?
        .ok_or(PersonIndexRebuildCancelError::Unavailable)
}

pub(crate) fn start_person_index_rebuild(people: &PeopleService) {
    people.schedule_person_index_rebuild();
}
