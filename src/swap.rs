pub struct Summary {
    pub venue: &'static str,
    pub input: String,
    pub expected: String,
    pub minimum: String,
    pub fee_budget: String,
    pub approvals: usize,
    pub token: &'static str,
    pub native: &'static str,
}
