use serde_json::{Value, json};

/// 訊息角色。工具結果歸在 User 側——三家 provider 都是這樣看的，
/// 只有 OpenAI 額外把它拆成 role="tool" 的獨立訊息
#[derive(Clone, Copy, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

/// 一則訊息裡的一個內容塊。這是各家格式的最小公倍數
#[derive(Clone)]
pub enum Block {
    Text(String),
    /// 模型要求呼叫工具。id 是給 OpenAI/Claude 配對結果用的，Gemini 改用 name 配對
    ToolCall {
        id: String,
        name: String,
        args: Value,
    },
    /// 工具執行結果。id 與 name 都帶著，因為各家配對方式不同
    ToolResult {
        id: String,
        name: String,
        content: String,
    },
    /// 某家 provider 專屬、且必須原封不動送回的塊（例如 Claude 的 thinking）。
    /// 產生它的 provider 認得它，其他 provider 直接跳過
    Opaque(Value),
}

pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

impl Message {
    pub fn user_text(text: &str) -> Self {
        Self {
            role: Role::User,
            blocks: vec![Block::Text(text.to_string())],
        }
    }

    pub fn assistant(blocks: Vec<Block>) -> Self {
        Self {
            role: Role::Assistant,
            blocks,
        }
    }

    /// 一輪的所有工具結果合成一則使用者訊息
    pub fn tool_results(blocks: Vec<Block>) -> Self {
        Self {
            role: Role::User,
            blocks,
        }
    }
}

/// 工具宣告的中性形式：直接帶 MCP 給的原始 JSON Schema，
/// 要不要清理、怎麼包，交給各 provider 自己決定
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

impl ToolDecl {
    /// 本地的 read_skill 工具（不來自任何 MCP server）
    pub fn read_skill() -> Self {
        Self {
            name: "read_skill".to_string(),
            description: "讀取一個技能的完整說明。執行技能相關任務前先呼叫這個。".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "技能名稱"}
                },
                "required": ["name"]
            }),
        }
    }
}

/// 從一輪回覆的內容塊裡挑出工具呼叫
pub fn tool_calls(blocks: &[Block]) -> Vec<(&str, &str, &Value)> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::ToolCall { id, name, args } => Some((id.as_str(), name.as_str(), args)),
            _ => None,
        })
        .collect()
}
