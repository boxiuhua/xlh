use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;

use crate::portfolio::Portfolio;
use crate::result::{DailyRecord, TradeRecord};

/// Metadata about the backtest, passed by caller.
pub struct ReportMeta {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: String,
    pub strategy_desc: String,
    pub initial_cash: f64,
}

// ──────────────────── JSON payload (serialised into the page) ────────────────

#[derive(Serialize)]
struct MetaJson<'a> {
    fund_code: &'a str,
    start: String,
    end: String,
    strategy_desc: &'a str,
}

#[derive(Serialize)]
struct MetricsJson {
    total_return: f64,
    annualized: f64,
    max_drawdown: f64,
    sharpe: f64,
    final_equity: f64,
    total_contributed: f64,
    trade_count: usize,
}

#[derive(Serialize)]
struct Payload<'a> {
    meta: MetaJson<'a>,
    metrics: MetricsJson,
    daily: &'a [DailyRecord],
    trades: &'a [TradeRecord],
}

// ──────────────────── public entry-point ─────────────────────────────────────

/// 渲染单次回测报告为完整自包含 HTML 字符串。
pub fn render_report_html(
    meta: &ReportMeta,
    pf: &Portfolio,
    daily: &[DailyRecord],
    trades: &[TradeRecord],
) -> String {
    let s = crate::metrics::summarize(pf, trades.len());
    let payload = Payload {
        meta: MetaJson {
            fund_code: &meta.fund_code,
            start: meta.start.to_string(),
            end: meta.end.to_string(),
            strategy_desc: &meta.strategy_desc,
        },
        metrics: MetricsJson {
            total_return: s.total_return,
            annualized: s.annualized,
            max_drawdown: s.max_drawdown,
            sharpe: s.sharpe,
            final_equity: s.final_equity,
            total_contributed: s.total_contributed,
            trade_count: s.trade_count,
        },
        daily,
        trades,
    };
    let data_json = serde_json::to_string(&payload)
        .expect("报告数据序列化失败")
        .replace("</", "<\\/");
    build_html(meta, &payload.metrics, trades, &data_json)
}

/// 渲染单次回测报告并写入 `out_dir/report.html`，返回写入的文件路径。
pub fn render_report(
    meta: &ReportMeta,
    pf: &Portfolio,
    daily: &[DailyRecord],
    trades: &[TradeRecord],
    out_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录 {} 失败", out_dir.display()))?;
    let html = render_report_html(meta, pf, daily, trades);
    let out_path = out_dir.join("report.html");
    std::fs::write(&out_path, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out_path.display()))?;
    Ok(out_path)
}

// ──────────────────── HTML construction ──────────────────────────────────────

fn fmt_pct(v: f64) -> String {
    format!("{:.2}%", v * 100.0)
}

fn sign_class(v: f64) -> &'static str {
    if v >= 0.0 { "pos" } else { "neg" }
}

fn build_html(meta: &ReportMeta, m: &MetricsJson, trades: &[TradeRecord], data_json: &str) -> String {
    // ── metric cards (server-rendered) ──────────────────────────────────────
    let metrics_cards = format!(
        r#"<div class="card metric-grid">
  <div class="metric"><span class="label">总收益</span><span class="value {c_ret}">{ret}</span></div>
  <div class="metric"><span class="label">年化收益(XIRR)</span><span class="value {c_ann}">{ann}</span></div>
  <div class="metric"><span class="label">最大回撤</span><span class="value neg">-{mdd}</span></div>
  <div class="metric"><span class="label">夏普比率</span><span class="value">{sharpe:.2}</span></div>
  <div class="metric"><span class="label">期末市值</span><span class="value">{equity:.2}</span></div>
  <div class="metric"><span class="label">累计投入</span><span class="value">{contrib:.2}</span></div>
  <div class="metric"><span class="label">成交笔数</span><span class="value">{tc}</span></div>
</div>"#,
        c_ret = sign_class(m.total_return),
        ret = fmt_pct(m.total_return),
        c_ann = sign_class(m.annualized),
        ann = fmt_pct(m.annualized),
        mdd = fmt_pct(m.max_drawdown),
        sharpe = m.sharpe,
        equity = m.final_equity,
        contrib = m.total_contributed,
        tc = m.trade_count,
    );

    // ── trades table (server-rendered) ──────────────────────────────────────
    let trade_rows: String = trades.iter().map(|t| {
        let (dir_text, dir_class) = match t.direction {
            crate::event::Direction::Buy  => ("买入", "buy"),
            crate::event::Direction::Sell => ("卖出", "sell"),
        };
        let amount = t.shares * t.price;
        format!(
            "<tr><td>{date}</td><td class=\"{dc}\">{dir}</td><td>{sh:.4}</td><td>{pr:.4}</td><td>{fee:.4}</td><td>{amt:.2}</td></tr>\n",
            date = t.date,
            dc   = dir_class,
            dir  = dir_text,
            sh   = t.shares,
            pr   = t.price,
            fee  = t.fee,
            amt  = amount,
        )
    }).collect();

    let trade_count = trades.len();

    let fund_code_esc     = super::html_escape(&meta.fund_code);
    let strategy_desc_esc = super::html_escape(&meta.strategy_desc);

    format!(r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>{fund_code_esc} 回测报告</title>
<style>
*,*::before,*::after{{box-sizing:border-box;margin:0;padding:0}}
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;background:#f5f6fa;color:#2c3e50;line-height:1.5}}
.container{{max-width:1100px;margin:0 auto;padding:24px 16px}}
header{{margin-bottom:24px}}
header h1{{font-size:1.8rem;font-weight:700;color:#1a252f}}
header .subtitle{{color:#7f8c8d;margin-top:4px;font-size:.95rem}}
.card{{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:20px;margin-bottom:20px;box-shadow:0 1px 4px rgba(0,0,0,.06)}}
.card h2{{font-size:1rem;font-weight:600;color:#34495e;margin-bottom:14px;border-bottom:1px solid #eaecef;padding-bottom:8px}}
.metric-grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:16px}}
.metric{{display:flex;flex-direction:column;gap:4px}}
.metric .label{{font-size:.78rem;color:#7f8c8d;text-transform:uppercase;letter-spacing:.04em}}
.metric .value{{font-size:1.35rem;font-weight:700}}
.pos{{color:#c0392b}}   /* A股惯例：盈利=红 */
.neg{{color:#27ae60}}   /* 亏损=绿 */
.chart{{width:100%;height:360px}}
table{{width:100%;border-collapse:collapse;font-size:.88rem}}
th{{background:#f0f2f5;text-align:left;padding:8px 10px;font-weight:600;color:#5a6a7a;position:sticky;top:0}}
td{{padding:7px 10px;border-bottom:1px solid #f0f2f5}}
tr:last-child td{{border-bottom:none}}
td.buy{{color:#c0392b;font-weight:600}}
td.sell{{color:#27ae60;font-weight:600}}
.scrollable{{max-height:320px;overflow-y:auto}}
.trade-count{{font-size:.82rem;color:#7f8c8d;margin-bottom:8px}}
</style>
</head>
<body>
<div class="container">
<header>
  <h1>{fund_code_esc} · 回测分析报告</h1>
  <div class="subtitle">回测区间：{start} ~ {end} &nbsp;|&nbsp; 策略：{strategy_desc_esc}</div>
</header>

<!-- Metric cards -->
<div class="card">
  <h2>核心指标</h2>
  {metrics_cards}
</div>

<!-- Chart 1: equity + contribution -->
<div class="card">
  <h2>资产走势</h2>
  <div id="chart-equity" class="chart"></div>
</div>

<!-- Chart 2: drawdown -->
<div class="card">
  <h2>回撤（水下曲线）</h2>
  <div id="chart-drawdown" class="chart"></div>
</div>

<!-- Chart 3: nav + adj_nav + trades -->
<div class="card">
  <h2>净值走势 &amp; 买卖点</h2>
  <div id="chart-nav" class="chart"></div>
</div>

<!-- Trades table -->
<div class="card">
  <h2>成交流水</h2>
  <p class="trade-count">共 {trade_count} 笔</p>
  <div class="scrollable">
  <table>
    <thead><tr><th>日期</th><th>方向</th><th>份额</th><th>价格</th><th>费用</th><th>金额</th></tr></thead>
    <tbody>
{trade_rows}    </tbody>
  </table>
  </div>
</div>

</div><!-- /container -->

<script>const DATA = {data_json};</script>
<script src="https://cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js"></script>
<script>
(function(){{
  if (typeof echarts === 'undefined') {{
    document.querySelectorAll('.chart').forEach(function(e){{
      e.style.display='flex';e.style.alignItems='center';e.style.justifyContent='center';
      e.style.color='#888';e.style.fontSize='14px';
      e.innerHTML='图表库加载失败（需联网加载 ECharts）';
    }});
    return;
  }}

  var daily  = DATA.daily;
  var trades = DATA.trades;

  // ── shared helpers ────────────────────────────────────────────────────────
  var dates      = daily.map(function(r){{ return r.date; }});
  var equities   = daily.map(function(r){{ return +r.equity.toFixed(4); }});

  // cumulative contribution per day
  var cumContrib = [];
  var running = 0;
  for (var i = 0; i < daily.length; i++) {{
    running += daily[i].contribution;
    cumContrib.push(+running.toFixed(4));
  }}

  var commonDataZoom = [
    {{ type:'slider', xAxisIndex:0, bottom:8, height:20 }},
    {{ type:'inside', xAxisIndex:0 }}
  ];

  // ── Chart 1: equity + cumulative contribution ─────────────────────────────
  var c1 = echarts.init(document.getElementById('chart-equity'));
  c1.setOption({{
    tooltip:{{ trigger:'axis', axisPointer:{{ type:'cross' }} }},
    legend:{{ data:['总市值','累计投入'] }},
    grid:{{ left:60, right:20, top:40, bottom:60 }},
    xAxis:{{ type:'category', data:dates, boundaryGap:false }},
    yAxis:{{ type:'value', name:'元', nameTextStyle:{{padding:[0,0,0,30]}} }},
    dataZoom: commonDataZoom,
    series:[
      {{ name:'总市值', type:'line', data:equities, smooth:true,
         lineStyle:{{color:'#c0392b'}}, itemStyle:{{color:'#c0392b'}},
         areaStyle:{{color:'rgba(192,57,43,.08)'}} }},
      {{ name:'累计投入', type:'line', data:cumContrib, smooth:true,
         lineStyle:{{color:'#2980b9',type:'dashed'}}, itemStyle:{{color:'#2980b9'}} }}
    ]
  }});

  // ── Chart 2: drawdown (underwater) ───────────────────────────────────────
  var ddData = (function(){{
    var peak = -Infinity;
    return equities.map(function(v){{
      if (v > peak) peak = v;
      return peak > 0 ? +((v / peak - 1) * 100).toFixed(4) : 0;
    }});
  }})();

  var c2 = echarts.init(document.getElementById('chart-drawdown'));
  c2.setOption({{
    tooltip:{{ trigger:'axis', valueFormatter: function(v){{ return v.toFixed(2)+'%'; }} }},
    grid:{{ left:60, right:20, top:40, bottom:60 }},
    xAxis:{{ type:'category', data:dates, boundaryGap:false }},
    yAxis:{{ type:'value', name:'%', axisLabel:{{ formatter:function(v){{return v+'%';}} }} }},
    dataZoom: commonDataZoom,
    series:[{{
      name:'回撤',
      type:'line',
      data:ddData,
      smooth:true,
      lineStyle:{{color:'#27ae60'}},
      itemStyle:{{color:'#27ae60'}},
      areaStyle:{{color:'rgba(39,174,96,.15)'}},
      symbol:'none'
    }}]
  }});

  // ── Chart 3: nav + adj_nav + trade marks ─────────────────────────────────
  var navData    = daily.map(function(r){{ return +r.nav.toFixed(6); }});
  var adjNavData = daily.map(function(r){{ return +r.adj_nav.toFixed(6); }});

  // Build a date→adj_nav lookup for trade overlays
  var adjNavMap = {{}};
  for (var i = 0; i < daily.length; i++) {{
    adjNavMap[daily[i].date] = daily[i].adj_nav;
  }}

  var buyPoints  = [];
  var sellPoints = [];
  trades.forEach(function(t){{
    var price = adjNavMap[t.date] !== undefined ? adjNavMap[t.date] : t.price;
    var pt = [t.date, +price.toFixed(6)];
    if (t.direction === 'buy')  buyPoints.push(pt);
    else                        sellPoints.push(pt);
  }});

  var c3 = echarts.init(document.getElementById('chart-nav'));
  c3.setOption({{
    tooltip:{{ trigger:'axis', axisPointer:{{ type:'cross' }} }},
    legend:{{ data:['单位净值','复权净值','买入','卖出'] }},
    grid:{{ left:60, right:20, top:40, bottom:60 }},
    xAxis:{{ type:'category', data:dates, boundaryGap:false }},
    yAxis:{{ type:'value', name:'净值' }},
    dataZoom: commonDataZoom,
    series:[
      {{ name:'单位净值', type:'line', data:navData, smooth:true,
         lineStyle:{{color:'#8e44ad'}}, itemStyle:{{color:'#8e44ad'}}, symbol:'none' }},
      {{ name:'复权净值', type:'line', data:adjNavData, smooth:true,
         lineStyle:{{color:'#e67e22'}}, itemStyle:{{color:'#e67e22'}}, symbol:'none' }},
      {{ name:'买入', type:'scatter', data:buyPoints,
         symbol:'triangle', symbolSize:10,
         itemStyle:{{color:'#c0392b'}},
         tooltip:{{ formatter: function(p){{ return '买入 '+p.data[0]+' @ '+p.data[1]; }} }} }},
      {{ name:'卖出', type:'scatter', data:sellPoints,
         symbol:'triangle', symbolSize:10, symbolRotate:180,
         itemStyle:{{color:'#27ae60'}},
         tooltip:{{ formatter: function(p){{ return '卖出 '+p.data[0]+' @ '+p.data[1]; }} }} }}
    ]
  }});

  // Resize charts on window resize
  window.addEventListener('resize', function(){{
    c1.resize(); c2.resize(); c3.resize();
  }});
}})();
</script>
</body>
</html>
"#,
        fund_code_esc     = fund_code_esc,
        start             = meta.start,
        end               = meta.end,
        strategy_desc_esc = strategy_desc_esc,
        metrics_cards     = metrics_cards,
        trade_count       = trade_count,
        trade_rows        = trade_rows,
        data_json         = data_json,
    )
}

// ──────────────────── tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use crate::event::Direction;
    use crate::portfolio::{EquityPoint, Portfolio};
    use crate::result::{DailyRecord, TradeRecord};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn make_pf() -> Portfolio {
        let mut pf = Portfolio::new(0.0);
        pf.total_contributed = 2000.0;
        pf.curve = vec![
            EquityPoint { date: d(2024, 1, 1), equity: 1000.0, contribution: 1000.0 },
            EquityPoint { date: d(2024, 2, 1), equity: 2000.0, contribution: 1000.0 },
            EquityPoint { date: d(2024, 2, 15), equity: 4000.0, contribution: 0.0 },
        ];
        pf.flows = vec![(d(2024, 1, 1), -1000.0), (d(2024, 2, 1), -1000.0), (d(2024, 2, 15), 4000.0)];
        pf
    }

    fn make_daily() -> Vec<DailyRecord> {
        vec![
            DailyRecord { date: d(2024, 1, 1),  nav: 1.0, adj_nav: 1.0, equity: 1000.0, contribution: 1000.0, shares: 1000.0, cash: 0.0 },
            DailyRecord { date: d(2024, 2, 1),  nav: 1.0, adj_nav: 1.0, equity: 2000.0, contribution: 1000.0, shares: 2000.0, cash: 0.0 },
            DailyRecord { date: d(2024, 2, 15), nav: 2.0, adj_nav: 2.0, equity: 4000.0, contribution: 0.0,    shares: 2000.0, cash: 0.0 },
        ]
    }

    fn make_trades() -> Vec<TradeRecord> {
        vec![
            TradeRecord { date: d(2024, 1, 1), direction: Direction::Buy, shares: 1000.0, price: 1.0, fee: 0.0 },
            TradeRecord { date: d(2024, 2, 1), direction: Direction::Buy, shares: 1000.0, price: 1.0, fee: 0.0 },
        ]
    }

    #[test]
    fn renders_self_contained_report() {
        let pf = make_pf();
        let daily = make_daily();
        let trades = make_trades();

        let meta = ReportMeta {
            fund_code: "161725".to_string(),
            start: d(2024, 1, 1),
            end: d(2024, 2, 15),
            strategy: "dca".to_string(),
            strategy_desc: "dca monthly 1 1000".to_string(),
            initial_cash: 0.0,
        };

        let tmp = std::env::temp_dir().join("xlh_html_test");
        let path = render_report(&meta, &pf, &daily, &trades, &tmp).unwrap();

        assert!(path.exists(), "report.html should exist");

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("const DATA"), "should embed const DATA");
        assert!(content.contains("161725"), "should contain fund code");
        assert!(content.contains("总收益"), "should contain 总收益 label");
        assert!(content.contains("最大回撤"), "should contain 最大回撤 label");
        assert!(content.contains("echarts"), "should reference echarts");
        // Trades are embedded in data JSON and in rendered HTML rows
        assert!(content.contains("buy") || content.contains("买入"), "should contain a trade entry");

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn html_escape_helper() {
        use crate::report::html_escape;
        assert_eq!(html_escape("a & b"), "a &amp; b");
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
        assert_eq!(html_escape("\"quoted\""), "&quot;quoted&quot;");
        assert_eq!(html_escape("it's"), "it&#39;s");
        // & must be escaped before < to avoid double-escaping
        assert_eq!(html_escape("a&<b"), "a&amp;&lt;b");
    }

    #[test]
    fn fund_code_with_angle_brackets_is_escaped_in_html() {
        let pf = make_pf();
        let daily = make_daily();
        let trades = make_trades();

        let meta = ReportMeta {
            fund_code: "<XSS>".to_string(),
            start: d(2024, 1, 1),
            end: d(2024, 2, 15),
            strategy: "dca".to_string(),
            strategy_desc: "dca & params".to_string(),
            initial_cash: 0.0,
        };

        let tmp = std::env::temp_dir().join("xlh_html_escape_test");
        let path = render_report(&meta, &pf, &daily, &trades, &tmp).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();

        // The HTML markup (title, h1, subtitle) must use escaped forms.
        // Note: <XSS> (without `/`) may appear inside the <script> DATA block
        // because serde_json does not escape `<`; that is safe since it's not
        // a closing tag. What matters is that the HTML body markup is escaped.
        assert!(content.contains("&lt;XSS&gt;"), "escaped <XSS> must appear in HTML markup");
        assert!(content.contains("dca &amp; params"), "escaped & must appear in HTML markup");

        // The title and h1 must not contain unescaped angle brackets
        // (check the title tag specifically)
        let title_start = content.find("<title>").expect("title tag");
        let title_end   = content.find("</title>").expect("/title tag");
        let title_text  = &content[title_start..title_end];
        assert!(!title_text.contains("<XSS>"), "raw <XSS> must not appear inside <title>");
        assert!(title_text.contains("&lt;XSS&gt;"), "<title> must contain escaped form");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_report_html_contains_core_markup() {
        use crate::portfolio::{Portfolio, EquityPoint};
        let mut pf = Portfolio::new(0.0);
        pf.curve.push(EquityPoint { date: d(2024,1,1), equity: 1000.0, contribution: 1000.0 });
        pf.curve.push(EquityPoint { date: d(2024,2,1), equity: 1500.0, contribution: 0.0 });
        pf.total_contributed = 1000.0;
        pf.flows = vec![(d(2024,1,1), -1000.0), (d(2024,2,1), 1500.0)];
        let meta = ReportMeta {
            fund_code: "161725".into(), start: d(2024,1,1), end: d(2024,2,1),
            strategy: "dca".into(), strategy_desc: "dca".into(), initial_cash: 0.0,
        };
        let html = render_report_html(&meta, &pf, &[], &[]);
        assert!(html.contains("const DATA"), "应内嵌 const DATA");
        assert!(html.contains("总收益"), "应含指标标签");
        assert!(html.contains("echarts"), "应引用 echarts");
    }

    #[test]
    fn script_injection_via_json_is_sanitized() {
        let pf = make_pf();
        let mut daily = make_daily();
        // Inject a </script> payload into a string field that ends up in the JSON
        daily[0].date = d(2024, 1, 1); // date is not injectable, but strategy_desc is
        let trades = make_trades();

        let meta = ReportMeta {
            fund_code: "161725".to_string(),
            start: d(2024, 1, 1),
            end: d(2024, 2, 15),
            strategy: "dca".to_string(),
            strategy_desc: "safe</script><script>alert(1)".to_string(),
            initial_cash: 0.0,
        };

        let tmp = std::env::temp_dir().join("xlh_html_script_test");
        let path = render_report(&meta, &pf, &daily, &trades, &tmp).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();

        // The raw `</script>` must not appear anywhere in the output
        assert!(!content.contains("</script><script>"), "unescaped </script> injection must not appear");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
