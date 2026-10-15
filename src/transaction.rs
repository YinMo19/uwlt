#[derive(Debug)]
pub struct Transfer {
    pub recipient: String,
    pub amount: String,
    pub symbol: &'static str,
}
