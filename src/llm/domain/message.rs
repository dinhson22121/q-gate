#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBlock {
    pub text: String,
    pub cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentPart {
    Text(String),
    Image { media_type: String, base64: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Structured<T> {
    pub value: T,
    pub usage: super::usage::Usage,
    pub cost_usd: f64,
}
