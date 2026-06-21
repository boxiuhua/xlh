use std::path::Path;
use anyhow::{anyhow, Context, Result};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FundInfo {
    pub code: String,
    pub name: String,
    pub pinyin: String,
}

/// 解析 fundcode_search.js：`var r = [["000001","HXCZHH","华夏成长混合","混合型","..."],...];`
/// 每条取 [0]=code、[1]=pinyin、[2]=name；列数不足的行跳过。
pub fn parse_fund_list(body: &str) -> Result<Vec<FundInfo>> {
    let key = "var r = ";
    let start = body.find(key).ok_or_else(|| anyhow!("未找到 var r ="))? + key.len();
    let rest = &body[start..];
    let open = rest.find('[').ok_or_else(|| anyhow!("未找到数组开括号"))?;
    let close = rest.rfind(']').ok_or_else(|| anyhow!("未找到数组闭括号"))?;
    if close < open { return Err(anyhow!("数组括号位置异常")); }
    let rows: Vec<Vec<String>> = serde_json::from_str(&rest[open..=close])
        .context("解析基金清单 JSON 失败")?;
    let funds = rows.into_iter()
        .filter(|r| r.len() >= 3)
        .map(|r| FundInfo { code: r[0].clone(), pinyin: r[1].clone(), name: r[2].clone() })
        .collect();
    Ok(funds)
}

/// 抓取天天基金全量清单。
pub fn fetch_fund_list() -> Result<Vec<FundInfo>> {
    let url = "https://fund.eastmoney.com/js/fundcode_search.js";
    let body = reqwest::blocking::Client::new()
        .get(url)
        .header("Referer", "https://fund.eastmoney.com/")
        .send()
        .map_err(|e| anyhow!("请求 {url} 失败: {e}"))?
        .text()
        .map_err(|e| anyhow!("读取 {url} 响应失败: {e}"))?;
    parse_fund_list(&body)
}

/// 缓存优先：cache_dir/fundlist.json 存在则读盘反序列化，否则抓取并写盘。
pub fn load_or_fetch_fund_list(cache_dir: &Path) -> Result<Vec<FundInfo>> {
    let path = cache_dir.join("fundlist.json");
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {} 失败", path.display()))?;
        return serde_json::from_str(&text)
            .with_context(|| format!("解析 {} 失败", path.display()));
    }
    let funds = fetch_fund_list()?;
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("创建缓存目录 {} 失败", cache_dir.display()))?;
    match serde_json::to_string(&funds) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                eprintln!("写入基金清单缓存 {} 失败: {e}", path.display());
            }
        }
        Err(e) => eprintln!("序列化基金清单失败: {e}"),
    }
    Ok(funds)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"var r = [["000001","HXCZHH","华夏成长混合","混合型-灵活","HUAXIACHENGZHANGHUNHE"],["161725","ZSZZBJ","招商中证白酒指数","指数型","ZHAOSHANGBAIJIU"],["bad"]];"#;

    #[test]
    fn parses_sample() {
        let funds = parse_fund_list(SAMPLE).unwrap();
        assert_eq!(funds.len(), 2, "列数不足的 [\"bad\"] 行应被跳过");
        assert_eq!(funds[0], FundInfo { code: "000001".into(), pinyin: "HXCZHH".into(), name: "华夏成长混合".into() });
        assert_eq!(funds[1].code, "161725");
        assert_eq!(funds[1].name, "招商中证白酒指数");
    }

    #[test]
    fn rejects_no_var_r() {
        assert!(parse_fund_list("garbage without array").is_err());
    }

    #[test]
    fn load_reads_cache_without_network() {
        // 写一份临时 fundlist.json，断言 load_or_fetch 读盘（不联网）
        let dir = std::env::temp_dir().join("xlh_fundlist_test");
        std::fs::create_dir_all(&dir).unwrap();
        let funds = vec![FundInfo { code: "161725".into(), name: "招商中证白酒指数".into(), pinyin: "ZSZZBJ".into() }];
        std::fs::write(dir.join("fundlist.json"), serde_json::to_string(&funds).unwrap()).unwrap();
        let loaded = load_or_fetch_fund_list(&dir).unwrap();
        assert_eq!(loaded, funds);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
