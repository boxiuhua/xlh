use std::path::PathBuf;
use anyhow::Result;
use clap::{Parser, Subcommand};

use xlh::broker::Broker;
use xlh::config;
use xlh::data::{cache, InMemoryData};
use xlh::engine::Engine;
use xlh::portfolio::Portfolio;
use xlh::report;

#[derive(Parser)]
#[command(name = "xlh", about = "A股基金定投/择时回测")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// 配置文件路径（无子命令时使用）
    #[arg(short, long, default_value = "config.toml", global = true)]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动本地 Web 界面
    Serve {
        /// 监听端口
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
    /// 定时推送持仓建议 + 基金诊断（钉钉/飞书/企业微信/Server酱）
    Push {
        /// 推送配置文件
        #[arg(long, default_value = "push.toml")]
        file: PathBuf,
        /// 立即跑一次即退出（否则按 cron 常驻守护）
        #[arg(long)]
        once: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Serve { port }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(xlh::web::serve(cli.config.clone(), port))?;
            Ok(())
        }
        Some(Commands::Push { file, once }) => {
            let cfg = xlh::push::load(&file)?;
            if once { xlh::push::run_once(&cfg) } else { xlh::push::run_daemon(&cfg) }
        }
        None => run_cli(&cli.config),
    }
}

fn run_cli(config: &std::path::Path) -> Result<()> {
    let cfg = config::load(config)?;

    if let Some(opt) = &cfg.optimize {
        if !cfg.compare.is_empty() {
            eprintln!("⚠ 同时存在 [optimize] 与 [[compare]]，本次按寻优模式执行，忽略 compare。");
        }
        let points = cache::load_or_fetch(&cfg.data.fund_code, &cfg.data.cache_dir, cfg.data.start, cfg.data.end)?;
        println!("加载 {} 条净值（{} ~ {}）", points.len(), cfg.data.start, cfg.data.end);
        let fee = config::build_fee(&cfg);
        let report = xlh::optimize::run_optimize(opt, &cfg.data.fund_code, &points, fee, cfg.portfolio.initial_cash)?;
        let show = report.top_n.min(report.ranked.len());
        println!("== 寻优 Top {} （按 {}）==", show, report.metric);
        for (i, o) in report.ranked.iter().take(show).enumerate() {
            println!("  {}. {}  总收益 {:.2}%  夏普 {:.2}  最大回撤 {:.2}%",
                i + 1, o.label,
                o.outcome.summary.total_return * 100.0,
                o.outcome.summary.sharpe,
                o.outcome.summary.max_drawdown * 100.0);
        }
        let meta = xlh::report::optimize::OptMeta {
            start: cfg.data.start, end: cfg.data.end, fund_code: cfg.data.fund_code.clone(),
        };
        let path = xlh::report::optimize::render_optimize(&meta, &report, &cfg.report.out_dir)?;
        println!("寻优报告已生成：{}", path.display());
        return Ok(());
    }

    if !cfg.compare.is_empty() {
        let mut runs = Vec::new();
        for run in &cfg.compare {
            let fund = run.fund_code.clone().unwrap_or_else(|| cfg.data.fund_code.clone());
            let points = cache::load_or_fetch(&fund, &cfg.data.cache_dir, cfg.data.start, cfg.data.end)
                .map_err(|e| anyhow::anyhow!("run [{}] 加载 {} 失败: {e}", run.name, fund))?;
            let strategy = config::build_strategy_from(&run.strategy.kind, &run.strategy.params, &run.rules)
                .map_err(|e| anyhow::anyhow!("run [{}] 构建策略失败: {e}", run.name))?;
            let fee = config::build_fee(&cfg);
            let outcome = xlh::runner::run_one(run.name.clone(), fund, points, strategy, fee, run.initial_cash);
            println!("✓ {}  总收益 {:.2}%  夏普 {:.2}", outcome.name, outcome.summary.total_return * 100.0, outcome.summary.sharpe);
            runs.push(outcome);
        }
        let meta = xlh::report::compare::CompareMeta { start: cfg.data.start, end: cfg.data.end };
        let path = xlh::report::compare::render_compare(&meta, &runs, &cfg.report.out_dir)?;
        println!("对比报告已生成：{}", path.display());
        return Ok(());
    }

    let points = cache::load_or_fetch(
        &cfg.data.fund_code,
        &cfg.data.cache_dir,
        cfg.data.start,
        cfg.data.end,
    )?;
    println!("加载 {} 条净值（{} ~ {}）", points.len(), cfg.data.start, cfg.data.end);

    let data = InMemoryData::new(points);
    let strategy = config::build_strategy(&cfg)?;
    let broker = Broker::new(config::build_fee(&cfg));
    let portfolio = Portfolio::new(cfg.portfolio.initial_cash);

    let mut engine = Engine::new(data, strategy, broker, portfolio);

    // Run the backtest — discard the &mut-borrowed return value immediately
    // so that subsequent &self borrows (portfolio/daily/trades) can coexist.
    engine.run();

    let pf = engine.portfolio();
    report::print_summary(pf);

    if cfg.report.chart {
        report::chart::render_equity(pf, &cfg.report.out_dir)?;
        println!("图表已保存到 {}/equity.png", cfg.report.out_dir.display());
    }
    if cfg.report.html {
        let meta = report::html::ReportMeta {
            fund_code: cfg.data.fund_code.clone(),
            start: cfg.data.start,
            end: cfg.data.end,
            strategy: cfg.strategy.kind.clone(),
            strategy_desc: {
                let params = cfg.strategy.params.as_ref().map(|v| v.to_string()).unwrap_or_default();
                if params.is_empty() {
                    cfg.strategy.kind.clone()
                } else {
                    format!("{} {}", cfg.strategy.kind, params)
                }
            },
            initial_cash: cfg.portfolio.initial_cash,
        };
        let path = report::html::render_report(
            &meta, pf, engine.daily(), engine.trades(), &cfg.report.out_dir,
        )?;
        println!("HTML 报告已生成：{}", path.display());
    }
    Ok(())
}
