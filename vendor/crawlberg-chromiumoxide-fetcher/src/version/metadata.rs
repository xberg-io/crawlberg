use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct KnownGoodVersions {
    pub versions: Vec<Version>,
}

#[derive(Debug, Deserialize)]
pub struct LastKnownGoodVersions {
    pub channels: HashMap<String, Version>,
}

#[derive(Debug, Deserialize)]
pub struct LatestPatchVersionsPerBuild {
    pub builds: HashMap<String, Version>,
}

#[derive(Debug, Deserialize)]
pub struct LatestVersionsPerMilestone {
    pub milestones: HashMap<String, Version>,
}

#[derive(Debug, Deserialize)]
pub struct Version {
    pub version: String,
    pub revision: String,
}
