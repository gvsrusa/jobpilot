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
    ReadMatches,
    WriteJobs,
    WriteMatches,
    WriteCompanyNotes,
    WriteJobFields,
    /// Reads a connector's declared endpoints, through the `Fetcher` only: no other socket.
    NetConnector,
    /// Open a posting's link in the owner's own browser. No automation.
    ApplyOpenLink,
    /// Show or copy prepared files and approved text to the owner.
    ApplySupply,
    BrowserDrive,
    BrowserFill,
    BrowserUpload,
    BrowserSubmit,
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

static DISCOVERY: AgentSpec = AgentSpec {
    agent: Agent::Discovery,
    permissions: &[NetConnector, ReadJobText, WriteJobs],
    run_timeout: Duration::from_secs(10 * 60),
};

static MATCHING: AgentSpec = AgentSpec {
    agent: Agent::Matching,
    permissions: &[
        ReadJobText,
        ReadProfileBasic,
        ReadResumeText,
        Model(TaskType::ParseJob),
        Model(TaskType::SemanticMatch),
        WriteJobFields,
        WriteMatches,
    ],
    run_timeout: Duration::from_secs(30 * 60),
};

static RESEARCH: AgentSpec = AgentSpec {
    agent: Agent::Research,
    permissions: &[
        ReadJobText,
        ReadCompanyText,
        Model(TaskType::SummarizeCompany),
        WriteCompanyNotes,
    ],
    run_timeout: Duration::from_secs(10 * 60),
};

/// What the Claude Code apply session may hold. A mode's narrower set is `permissions::decide`'s
/// job, not the runner's.
static CLAUDE_APPLY: AgentSpec = AgentSpec {
    agent: Agent::ClaudeApply,
    permissions: &[
        ReadProfileBasic,
        ReadCareerMemory,
        ReadSensitive,
        ReadApplications,
        WriteApplications,
        WriteCareerMemory,
        ReadJobText,
        BrowserDrive,
        BrowserFill,
        BrowserUpload,
        BrowserSubmit,
    ],
    run_timeout: Duration::from_secs(8 * 60 * 60),
};

static BROWSER_APPLICATION: AgentSpec = AgentSpec {
    agent: Agent::BrowserApplication,
    permissions: &[
        ReadProfileBasic,
        ReadCareerMemory,
        ReadSensitive,
        ReadApplications,
        WriteApplications,
        WriteCareerMemory,
        ReadJobText,
        BrowserDrive,
        BrowserFill,
        BrowserUpload,
        BrowserSubmit,
        ApplyOpenLink,
        ApplySupply,
        Model(TaskType::MapFormField),
    ],
    run_timeout: Duration::from_secs(2 * 60 * 60),
};

pub fn spec_of(agent: Agent) -> &'static AgentSpec {
    match agent {
        Agent::Resume => &RESUME,
        Agent::CoverLetter => &COVER_LETTER,
        Agent::CareerMemory => &CAREER_MEMORY,
        Agent::Tracking => &TRACKING,
        Agent::Analytics => &ANALYTICS,
        Agent::Discovery => &DISCOVERY,
        Agent::Matching => &MATCHING,
        Agent::Research => &RESEARCH,
        Agent::ClaudeApply => &CLAUDE_APPLY,
        Agent::BrowserApplication => &BROWSER_APPLICATION,
    }
}
