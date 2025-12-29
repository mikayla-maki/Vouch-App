use gpui::SharedString;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug)]
pub struct Contact {
    pub id: ContactId,
    pub petname: SharedString,
    pub public_key: SharedString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContactId(pub u64);

#[derive(Clone, Debug)]
pub struct RecordSource {
    pub original_author: ContactId,
    pub received_via: Option<ContactId>,
    pub is_revouch: bool,
    pub revouched_by: Vec<ContactId>,
    pub timestamp: SystemTime,
}

#[derive(Clone, Debug)]
pub struct Recommendation {
    pub id: RecommendationId,
    pub subject_name: SharedString,
    pub content: SharedString,
    pub source: RecordSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RecommendationId(pub u64);

#[derive(Clone)]
pub struct MockData {
    pub contacts: Vec<Contact>,
    pub recommendations: Vec<Recommendation>,
}

impl MockData {
    pub fn generate() -> Self {
        let contacts = vec![
            Contact {
                id: ContactId(0),
                petname: "You".into(),
                public_key: "local_user_key".into(),
            },
            Contact {
                id: ContactId(1),
                petname: "Mom".into(),
                public_key: "mom_public_key_abc123".into(),
            },
            Contact {
                id: ContactId(2),
                petname: "Alice".into(),
                public_key: "alice_public_key_def456".into(),
            },
            Contact {
                id: ContactId(3),
                petname: "Bob".into(),
                public_key: "bob_public_key_ghi789".into(),
            },
            Contact {
                id: ContactId(4),
                petname: "Sam".into(),
                public_key: "sam_public_key_jkl012".into(),
            },
        ];

        let now = SystemTime::now();

        let recommendations = vec![
            Recommendation {
                id: RecommendationId(1),
                subject_name: "Thai Place Downtown".into(),
                content: "Best pad thai I've ever had! The atmosphere is cozy and the staff is incredibly friendly. Definitely get the mango sticky rice for dessert.".into(),
                source: RecordSource {
                    original_author: ContactId(1),
                    received_via: None,
                    is_revouch: false,
                    revouched_by: vec![ContactId(0), ContactId(2)],
                    timestamp: now - Duration::from_secs(2 * 60 * 60),
                },
            },
            Recommendation {
                id: RecommendationId(2),
                subject_name: "John's Auto Repair".into(),
                content: "Avoid this place! They overcharged me by $200 and didn't even fix the original problem. Had to take my car somewhere else afterward.".into(),
                source: RecordSource {
                    original_author: ContactId(2),
                    received_via: None,
                    is_revouch: false,
                    revouched_by: vec![],
                    timestamp: now - Duration::from_secs(24 * 60 * 60),
                },
            },
            Recommendation {
                id: RecommendationId(3),
                subject_name: "Dr. Sarah Chen, Dentist".into(),
                content: "So gentle and patient! I used to be terrified of the dentist but she made me feel completely at ease. Her office is spotless and modern.".into(),
                source: RecordSource {
                    original_author: ContactId(0),
                    received_via: None,
                    is_revouch: false,
                    revouched_by: vec![ContactId(1)],
                    timestamp: now - Duration::from_secs(3 * 24 * 60 * 60),
                },
            },
            Recommendation {
                id: RecommendationId(4),
                subject_name: "Sunset Hiking Trail".into(),
                content: "Beautiful 3-mile loop with amazing views of the valley. Best at golden hour. Bring water - there's no shade in the middle section.".into(),
                source: RecordSource {
                    original_author: ContactId(3),
                    received_via: Some(ContactId(2)),
                    is_revouch: true,
                    revouched_by: vec![],
                    timestamp: now - Duration::from_secs(5 * 24 * 60 * 60),
                },
            },
            Recommendation {
                id: RecommendationId(5),
                subject_name: "Margot's Bakery".into(),
                content: "The croissants are perfection - flaky, buttery, and they make them fresh every morning. Get there early, they sell out fast!".into(),
                source: RecordSource {
                    original_author: ContactId(4),
                    received_via: Some(ContactId(1)),
                    is_revouch: true,
                    revouched_by: vec![ContactId(0)],
                    timestamp: now - Duration::from_secs(7 * 24 * 60 * 60),
                },
            },
            Recommendation {
                id: RecommendationId(6),
                subject_name: "City Library Main Branch".into(),
                content: "Quiet study rooms, great wifi, and the librarians are super helpful. They have a wonderful children's section too.".into(),
                source: RecordSource {
                    original_author: ContactId(0),
                    received_via: None,
                    is_revouch: false,
                    revouched_by: vec![],
                    timestamp: now - Duration::from_secs(10 * 24 * 60 * 60),
                },
            },
            Recommendation {
                id: RecommendationId(7),
                subject_name: "Green Thumb Plant Shop".into(),
                content: "Amazing selection of houseplants and the owner gives great care advice. Prices are reasonable and plants arrive healthy.".into(),
                source: RecordSource {
                    original_author: ContactId(1),
                    received_via: None,
                    is_revouch: false,
                    revouched_by: vec![ContactId(2), ContactId(3)],
                    timestamp: now - Duration::from_secs(14 * 24 * 60 * 60),
                },
            },
        ];

        Self {
            contacts,
            recommendations,
        }
    }

    pub fn get_contact(&self, id: ContactId) -> Option<&Contact> {
        self.contacts.iter().find(|c| c.id == id)
    }

    pub fn get_contact_name(&self, id: ContactId) -> SharedString {
        self.get_contact(id)
            .map(|c| c.petname.clone())
            .unwrap_or_else(|| "Unknown".into())
    }

    pub fn local_user_id() -> ContactId {
        ContactId(0)
    }
}
