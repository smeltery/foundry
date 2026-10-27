//! Bound typed ingress without serializing any incoming secret container.
use super::*;
pub(super) fn bound(document: &TeamDocument) -> Result<()> {
    let count = document.admissions.len()
        + document.members.len()
        + document.invites.len()
        + document.attestations.len();
    if count > 4096 {
        return Err(StoreError::TooLarge);
    }
    let mut total = count * 256;
    let mut add = |value: &str| -> Result<()> {
        total = total.checked_add(value.len()).ok_or(StoreError::TooLarge)?;
        if total > MAX_CHECKPOINT_BYTES {
            return Err(StoreError::TooLarge);
        }
        Ok(())
    };
    add(&document.id)?;
    add(&document.meta.sig)?;
    add(&document.meta.body.name)?;
    if let Some(relay) = &document.meta.body.relay {
        add(relay)?;
    }
    for (id, record) in document.admissions.iter().chain(&document.members) {
        add(id)?;
        add(&record.sig)?;
        add(&record.body.person)?;
        add(&record.body.machine)?;
    }
    for (id, record) in &document.invites {
        add(id)?;
        add(&record.sig)?;
        add(&record.body.hash)?;
        if let Some(person) = &record.body.person {
            add(person)?;
        }
    }
    if let Some(link) = &document.link {
        add(&link.sig)?;
        add(&link.body.secret)?;
    }
    for (id, token) in &document.attestations {
        add(id)?;
        add(token)?;
    }
    Ok(())
}
