use anyhow::{anyhow, Result};
use chrono::{FixedOffset, TimeZone};
use serde::Deserialize;
use crate::data::NavPoint;

#[derive(Deserialize)]
struct NetWorth { x: i64, y: f64 }

/// 从 body 中截取 `var <name> = <array>;` 的数组文本。
/// 通过括号深度计数定位匹配的 `]`，忽略双引号字符串内的括号（支持 `\` 转义），
/// 避免 JSON 字符串值中含有 `];` 时误截断。
fn extract_array(body: &str, name: &str) -> Result<String> {
    let key = format!("var {name} = ");
    let start = body.find(&key).ok_or_else(|| anyhow!("未找到 {name}"))? + key.len();
    let rest = &body[start..];

    // 找到开头的 '['
    let open = rest.find('[').ok_or_else(|| anyhow!("{name} 未找到开括号 ["))?;
    let chars: Vec<char> = rest[open..].chars().collect();

    let mut depth: usize = 0;
    let mut in_string = false;
    let mut escape = false;
    let mut close_pos: Option<usize> = None;  // byte offset relative to rest[open..]

    let mut byte_offset = 0usize;
    for &ch in &chars {
        if escape {
            escape = false;
        } else if in_string {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        close_pos = Some(byte_offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        byte_offset += ch.len_utf8();
    }

    let close = close_pos.ok_or_else(|| anyhow!("{name} 数组未闭合"))?;
    // rest[open .. open+close+1] gives the full [...] substring
    let slice = &rest[open..open + close + ']'.len_utf8()];
    Ok(slice.to_string())
}

pub fn parse_pingzhongdata(body: &str) -> Result<Vec<NavPoint>> {
    let nw_text = extract_array(body, "Data_netWorthTrend")?;
    let ac_text = extract_array(body, "Data_ACWorthTrend")?;
    let nw: Vec<NetWorth> = serde_json::from_str(&nw_text)?;
    let ac: Vec<(i64, f64)> = serde_json::from_str(&ac_text)?;

    let mut acc_map = std::collections::HashMap::new();
    for (ts, v) in ac { acc_map.insert(ts, v); }

    let cst = FixedOffset::east_opt(8 * 3600).unwrap();

    let mut points = Vec::with_capacity(nw.len());
    for n in nw {
        let dt = cst.timestamp_millis_opt(n.x).single()
            .ok_or_else(|| anyhow!("非法时间戳 {}", n.x))?;
        let date = dt.date_naive();
        let acc_nav = *acc_map.get(&n.x).unwrap_or(&n.y);
        points.push(NavPoint { date, nav: n.y, acc_nav });
    }
    points.sort_by_key(|p| p.date);
    Ok(points)
}

/// 从天天基金 pingzhongdata 接口抓取全量净值。
pub fn fetch(code: &str) -> Result<Vec<NavPoint>> {
    let url = format!("https://fund.eastmoney.com/pingzhongdata/{code}.js");
    let body = reqwest::blocking::Client::new()
        .get(&url)
        .header("Referer", "https://fund.eastmoney.com/")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .map_err(|e| anyhow!("请求 {url} 失败: {e}"))?
        .text()
        .map_err(|e| anyhow!("读取响应失败: {e}"))?;
    parse_pingzhongdata(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    // 精简样本：模拟 pingzhongdata.js 中的两个数组
    // ts 1577808000000 = 2020-01-01 00:00:00 CST (UTC+8)
    // ts 1577894400000 = 2020-01-02 00:00:00 CST (UTC+8)
    const SAMPLE: &str = r#"
var fS_name = "测试基金";
var Data_netWorthTrend = [{"x":1577808000000,"y":1.0,"equityReturn":0,"unitMoney":""},{"x":1577894400000,"y":1.1,"equityReturn":10,"unitMoney":""}];
var Data_ACWorthTrend = [[1577808000000,1.0],[1577894400000,1.2]];
var Data_grandTotal = [];
"#;

    #[test]
    fn parses_nav_and_acc() {
        let pts = parse_pingzhongdata(SAMPLE).unwrap();
        assert_eq!(pts.len(), 2);
        assert!((pts[0].nav - 1.0).abs() < 1e-9);
        assert!((pts[1].nav - 1.1).abs() < 1e-9);
        assert!((pts[1].acc_nav - 1.2).abs() < 1e-9);
    }

    #[test]
    fn cst_date_regression() {
        // ts 1577808000000 must parse as 2020-01-01 in CST, NOT 2019-12-31
        let pts = parse_pingzhongdata(SAMPLE).unwrap();
        assert_eq!(pts[0].date, NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
            "首个数据点日期应为 2020-01-01 (CST), 实际得到 {}", pts[0].date);
        assert_eq!(pts[1].date, NaiveDate::from_ymd_opt(2020, 1, 2).unwrap(),
            "第二个数据点日期应为 2020-01-02 (CST), 实际得到 {}", pts[1].date);
    }

    #[test]
    fn extract_array_ignores_bracket_in_string() {
        // Ensure ]; inside a JSON string value does not cause mis-slicing
        let body = r#"var Data_netWorthTrend = [{"x":1,"y":1.0,"unitMoney":"[foo];bar"}];
var Data_ACWorthTrend = [[1,1.0]];"#;
        let arr = extract_array(body, "Data_netWorthTrend").unwrap();
        // Should parse cleanly as JSON
        let parsed: Vec<NetWorth> = serde_json::from_str(&arr).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].x, 1);
    }
}
