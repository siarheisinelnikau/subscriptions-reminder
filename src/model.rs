use crate::i18n::tr;
use chrono::{Days, Months, NaiveDate};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cycle {
    #[default]
    Monthly,
    Quarterly,
    Yearly,
    Weekly,
    OneTime,
}

impl Cycle {
    pub const ALL: [Self; 5] = [
        Self::Monthly,
        Self::Quarterly,
        Self::Yearly,
        Self::Weekly,
        Self::OneTime,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Monthly => tr("Monthly"),
            Self::Quarterly => tr("Quarterly"),
            Self::Yearly => tr("Yearly"),
            Self::Weekly => tr("Weekly"),
            Self::OneTime => tr("One time"),
        }
    }
    pub fn next(self, date: NaiveDate) -> Option<NaiveDate> {
        match self {
            Self::Monthly => date.checked_add_months(Months::new(1)),
            Self::Quarterly => date.checked_add_months(Months::new(3)),
            Self::Yearly => date.checked_add_months(Months::new(12)),
            Self::Weekly => date.checked_add_days(Days::new(7)),
            Self::OneTime => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: Uuid,
    pub name: String,
    pub due: NaiveDate,
    pub reminder_days: u32,
    pub cycle: Cycle,
    pub amount: String,
    pub currency: String,
    pub notes: String,
    pub active: bool,
}

impl Subscription {
    pub fn new(today: NaiveDate) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            due: today,
            reminder_days: 2,
            cycle: Cycle::Monthly,
            amount: String::new(),
            currency: "USD".into(),
            notes: String::new(),
            active: true,
        }
    }
    pub fn days_left(&self, today: NaiveDate) -> i64 {
        (self.due - today).num_days()
    }
    pub fn needs_reminder(&self, today: NaiveDate) -> bool {
        self.active && self.days_left(today) <= self.reminder_days as i64
    }
    pub fn renew(&mut self, today: NaiveDate) -> bool {
        if let Some(next) = self.cycle.next(self.due.max(today)) {
            self.due = next;
            true
        } else {
            false
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err(tr("Enter a subscription name.").into());
        }
        if self.name.chars().count() > 150 {
            return Err(tr("Use at most 150 characters for the name.").into());
        }
        if self.reminder_days > 365 {
            return Err(tr("Reminder lead time must be between 0 and 365 days.").into());
        }
        if !self.amount.trim().is_empty()
            && !self
                .amount
                .parse::<f64>()
                .is_ok_and(|v| v.is_finite() && v >= 0.0)
        {
            return Err(tr("Enter a positive price (for example 9.99), or leave it empty.").into());
        }
        if self.currency.len() > 12 {
            return Err(tr("Currency must be at most 12 characters.").into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Database {
    pub schema_version: u32,
    pub subscriptions: Vec<Subscription>,
}

impl Default for Database {
    fn default() -> Self {
        Self {
            schema_version: 1,
            subscriptions: vec![],
        }
    }
}

impl Database {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(
                "Unsupported data format. Update the application before opening this file.".into(),
            );
        }
        let mut ids = std::collections::HashSet::new();
        for sub in &self.subscriptions {
            sub.validate()?;
            if !ids.insert(sub.id) {
                return Err("The data file contains duplicate subscription IDs.".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Delivery {
    pub id: Uuid,
    pub due: Option<NaiveDate>,
    pub day: Option<NaiveDate>,
    pub windows: bool,
    pub telegram: bool,
}

pub fn was_delivered(
    deliveries: &[Delivery],
    sub: &Subscription,
    day: NaiveDate,
    telegram: bool,
) -> bool {
    deliveries.iter().any(|d| {
        d.id == sub.id
            && d.due == Some(sub.due)
            && d.day == Some(day)
            && if telegram { d.telegram } else { d.windows }
    })
}

pub fn reminder_text(sub: &Subscription, today: NaiveDate) -> String {
    let days = sub.days_left(today);
    let timing = if days < 0 {
        crate::trf!("expired {} day(s) ago", -days)
    } else if days == 0 {
        tr("expires today").into()
    } else {
        crate::trf!("expires in {} day(s)", days)
    };
    format!("{} {timing} ({}).", sub.name, sub.due)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    #[test]
    fn reminder_window_includes_late_start_and_overdue_but_excludes_paused() {
        let mut sub = Subscription::new(date("2026-11-02"));
        assert!(!sub.needs_reminder(date("2026-10-30")));
        assert!(sub.needs_reminder(date("2026-10-31")));
        assert!(sub.needs_reminder(date("2026-11-02")));
        assert!(sub.needs_reminder(date("2026-11-04")));
        sub.active = false;
        assert!(!sub.needs_reminder(date("2026-11-04")));
    }
    #[test]
    fn renewal_handles_month_end_leap_year_and_late_payment() {
        assert_eq!(
            Cycle::Monthly.next(date("2026-01-31")),
            Some(date("2026-02-28"))
        );
        assert_eq!(
            Cycle::Yearly.next(date("2024-02-29")),
            Some(date("2025-02-28"))
        );
        let mut sub = Subscription::new(date("2026-08-10"));
        assert!(sub.renew(date("2026-10-30")));
        assert_eq!(sub.due, date("2026-11-30"));
        sub.cycle = Cycle::OneTime;
        assert!(!sub.renew(date("2026-10-30")));
    }
    #[test]
    fn delivery_is_deduplicated_per_channel_date_and_expiry() {
        let mut sub = Subscription::new(date("2026-11-01"));
        let day = date("2026-10-30");
        let log = vec![Delivery {
            id: sub.id,
            due: Some(sub.due),
            day: Some(day),
            windows: true,
            telegram: false,
        }];
        assert!(was_delivered(&log, &sub, day, false));
        assert!(!was_delivered(&log, &sub, day, true));
        assert!(!was_delivered(&log, &sub, date("2026-10-31"), false));
        sub.renew(day);
        assert!(!was_delivered(&log, &sub, day, false));
    }
    #[test]
    fn invalid_data_is_rejected() {
        let mut sub = Subscription::new(date("2026-10-30"));
        assert!(sub.validate().is_err());
        sub.name = "Example".into();
        sub.amount = "NaN".into();
        assert!(sub.validate().is_err());
        sub.amount = "9.99".into();
        assert!(sub.validate().is_ok());
        assert!(
            Database {
                schema_version: 1,
                subscriptions: vec![sub.clone(), sub]
            }
            .validate()
            .is_err()
        );
    }
}
