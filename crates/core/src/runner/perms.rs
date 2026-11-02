//! What each agent may do. A set is fixed in code and has no user setting (FR-11.11).
//!
//! No variant exists for a shell, a secret value, a free path, free network access,
//! approving an answer, lowering a class, or answering a pause: no agent can be granted one.

use std::time::Duration;

use super::Agent;
use crate::ai::TaskType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ReadJobText,
    ReadCompanyText,
    ReadProfileBasic,
    ReadResumeText,
    ReadCareerMemory,
    ReadSensitive,
    ReadApplications,
    WriteDrafts,
    WriteCareerMemory,
    WriteApplications,
    WriteReports,
    Model(TaskType),
    EmbedLocal,
}

use Permission::*;

pub struct AgentSpec {
    pub agent: Agent,
    pub permissions: &'static [Permission],
    pub run_timeout: Duration,
}

static RESUME: AgentSpec = AgentSpec {
    agent: Agent::Resume,
    permissions: &[
        ReadResumeText,
        ReadProfileBasic,
        ReadJobText,
        Model(TaskType::DraftResume),
        WriteDrafts,
    ],
    run_timeout: Duration::from_secs(30 * 60),
};
static COVER_LETTER: AgentSpec = AgentSpec {
    agent: Agent::CoverLetter,
    permissions: &[
        ReadResumeText,
        ReadProfileBasic,
        ReadJobText,
        ReadCompanyText,
        Model(TaskType::DraftCoverLetter),
        WriteDrafts,
    ],
    run_timeout: Duration::from_secs(30 * 60),
};
static CAREER_MEMORY: AgentSpec = AgentSpec {
    agent: Agent::CareerMemory,
    permissions: &[
        ReadCareerMemory,
        ReadJobText,
        ReadSensitive,
        WriteCareerMemory,
        EmbedLocal,
        Model(TaskType::ClassifyAnswer),
    ],
    run_timeout: Duration::from_secs(10 * 60),
};
static TRACKING: AgentSpec = AgentSpec {
    agent: Agent::Tracking,
    permissions: &[ReadApplications, WriteApplications],
    run_timeout: Duration::from_secs(30),
};
static ANALYTICS: AgentSpec = AgentSpec {
    agent: Agent::Analytics,
    permissions: &[ReadApplications, WriteReports],
    run_timeout: Duration::from_secs(60),
};

pub fn spec_of(agent: Agent) -> &'static AgentSpec {
    match agent {
        Agent::Resume => &RESUME,
        Agent::CoverLetter => &COVER_LETTER,
        Agent::CareerMemory => &CAREER_MEMORY,
        Agent::Tracking => &TRACKING,
        Agent::Analytics => &ANALYTICS,
    }
}
