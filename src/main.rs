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
    /// 定时推送持仓建议（多用户；读主库中各用户配置）
    Push {
        /// 立即对所有授权用户跑一次即退出（否则按各自 cron 常驻守护）
        #[arg(long)]
        once: bool,
    },
    /// 创建管理员（密码经环境变量 XLH_ADMIN_PASSWORD 传入）
    Admin {
        #[command(subcommand)]
        action: AdminCmd,
    },
    /// 授权码管理
    License {
        #[command(subcommand)]
        action: LicenseCmd,
    },
    /// 列出用户
    User {
        #[command(subcommand)]
        action: UserCmd,
    },
    /// 盘中实时异动（守护会自动跑；此命令用于手动验证与排查）
    Realtime {
        #[command(subcommand)]
        action: RealtimeCmd,
    },
}

#[derive(Subcommand)]
enum RealtimeCmd {
    /// 立即抓一次快照并检测异动（忽略抓取时点限制，但仍守交易日判断）
    Once {
        /// 只抓前 N 只，用于快速验证（默认全市场）
        #[arg(long)]
        limit: Option<usize>,
    },
    /// 打印当日异动榜
    Movers {
        /// 日期 YYYY-MM-DD，默认今天
        #[arg(long)]
        day: Option<String>,
    },
    /// 回填结局并打印收盘汇总
    Summary {
        #[arg(long)]
        day: Option<String>,
    },
}

#[derive(Subcommand)]
enum AdminCmd {
    /// 创建管理员账号
    Create { #[arg(long)] username: String },
}

#[derive(Subcommand)]
enum LicenseCmd {
    /// 生成授权码
    Issue { #[arg(long)] days: i64, #[arg(long, default_value_t = 1)] count: u32 },
    /// 列出授权码
    List { #[arg(long, default_value = "unused")] filter: String },
}

#[derive(Subcommand)]
enum UserCmd {
    /// 列出所有用户
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Serve { port }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(xlh::web::serve(cli.config.clone(), port))?;
            Ok(())
        }
        Some(Commands::Push { once }) => {
            // 装载进程级实时配置：守护的实时抓取靠它，且必须尊重 --config
            if let Err(e) = xlh::stock::realtime::config::init(&cli.config) {
                eprintln!("⚠ [realtime] 配置无效，实时抓取未启用：{e}");
            }
            let auth_cfg = xlh::web::auth::config::load_auth(&cli.config);
            let conn = xlh::web::auth::store::open(&auth_cfg.db_path)?;
            xlh::history::migrate(&conn)?;
            xlh::push::store::migrate(&conn)?;
            xlh::push::store::migrate_legacy_push(&conn, std::path::Path::new("push.toml")).ok();
            if once {
                xlh::push::run_all_once(&conn, auth_cfg.warn_days, auth_cfg.grace_days)
            } else {
                xlh::push::run_multi_daemon(&conn, auth_cfg.warn_days, auth_cfg.grace_days)
            }
        }
        Some(Commands::Admin { action }) => match action {
            AdminCmd::Create { username } => xlh::web::auth::cli::admin_create(&cli.config, &username),
        },
        Some(Commands::License { action }) => match action {
            LicenseCmd::Issue { days, count } => xlh::web::auth::cli::license_issue(&cli.config, days, count),
            LicenseCmd::List { filter } => xlh::web::auth::cli::license_list(&cli.config, &filter),
        },
        Some(Commands::User { action }) => match action {
            UserCmd::List => xlh::web::auth::cli::user_list(&cli.config),
        },
        Some(Commands::Realtime { action }) => realtime_cmd(&cli.config, action),
        None => run_cli(&cli.config),
    }
}

fn realtime_cmd(config: &std::path::Path, action: RealtimeCmd) -> Result<()> {
    use xlh::stock::realtime::{config as rtcfg, job, store};
    // 走 init 而非 load_from_toml：与 serve/守护同一条装载路径，
    // 确保 --config 在所有入口一致生效
    let cfg = rtcfg::init(config)?;
    let mut conn = store::open(&cfg.db_path)?;
    let parse_day = |s: &Option<String>| -> Result<chrono::NaiveDate> {
        Ok(match s {
            Some(s) => chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")?,
            None => chrono::Local::now().date_naive(),
        })
    };

    match action {
        RealtimeCmd::Once { limit } => {
            let now = chrono::Local::now().naive_local();
            let date = xlh::stock::data::universe::latest_trade_date()
                .unwrap_or(now.date() - chrono::Duration::days(1));
            let listings = xlh::stock::data::universe::load_or_fetch(std::path::Path::new(".cache"), date)?;
            let mut symbols = job::a_share_symbols(&listings);
            let names = xlh::stock::data::universe::name_map(&listings);
            if let Some(n) = limit { symbols.truncate(n); }
            println!("抓取 {} 只（清单日 {}）…", symbols.len(), date);

            let out = job::run_tick(&mut conn, &cfg, &symbols, &names, now)?;
            println!("快照 {} 条，异动 {} 只，应推送 {} 只{}",
                out.ticks, out.movers.len(), out.pushed.len(),
                if out.flow_ok { "" } else { "（资金流不可用）" });
            if !out.movers.is_empty() {
                println!("\n{}", job::render_movers(&out.movers, out.flow_ok));
            }
            Ok(())
        }
        RealtimeCmd::Movers { day } => {
            let d = parse_day(&day)?;
            let rows = store::signals_on(&conn, d)?;
            println!("{}", job::render_summary(&rows, d));
            Ok(())
        }
        RealtimeCmd::Summary { day } => {
            let d = parse_day(&day)?;
            println!("{}", job::close_summary(&conn, d)?);
            Ok(())
        }
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
        println!("== 寻优 Top {} （按训练段 {} 排序）==", show, report.metric);
        for (i, o) in report.ranked.iter().take(show).enumerate() {
            let t = &o.outcome.summary;
            print!("  {}. {}\n     训练段(选参数用): 收益 {:.2}%  夏普 {:.2}  回撤 {:.2}%\n",
                i + 1, o.label,
                t.total_return * 100.0, t.sharpe, t.max_drawdown * 100.0);
            match &o.oos {
                Some(oo) => {
                    let s = &oo.summary;
                    println!("     检验段(样本外·看这个): 收益 {:.2}%  夏普 {:.2}  回撤 {:.2}%",
                        s.total_return * 100.0, s.sharpe, s.max_drawdown * 100.0);
                }
                None => println!("     检验段: 无（数据不足）"),
            }
        }
        // 警示必须打出来 —— 只写在 struct 里没人看见的警示等于没有
        println!("\n{}\n", report.caveat);
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
