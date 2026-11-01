//! Explicit synthetic acceptance against an already-configured provider.
//! Ordinary tests never read the live database or make this request.
use super::*;
use serde_json::json;

const SYNTHETIC_BRIEF: &str = "请实现一个用于整理CSV文件的命令行工具，输入为用户指定的目录，递归读取其中的CSV文件，验证日期、类别与金额三个必要字段，识别并报告缺失值和非法数字，按日期和类别计算汇总金额，并输出UTF-8编码的汇总CSV与简洁的错误报告。保持原始文件不变，不覆盖已有输出，允许用户指定输出目录，遇到单个错误文件时继续处理其他文件，最后给出成功与失败文件数量。提供安装说明、命令示例和覆盖正常输入、空目录、重复记录及格式错误的测试。实现前先说明合理假设，不添加用户没有要求的网络功能或服务。";

const SYNTHETIC_LONG_BRIEF: &str = "请在现有本地数据整理工具中实现三个相互配合的功能，首先提供一个命令行入口，接收用户指定的输入目录与输出目录，递归发现CSV文件并验证日期、类别、金额以及记录标识四个必要字段，将符合要求的记录按日期和类别汇总到UTF-8编码的输出文件，同时将缺失字段、无效日期、非法数字和重复记录写入单独的错误报告；任何时候都必须保留原始输入文件，默认不得覆盖已有输出，遇到单个损坏文件时应继续处理其他文件，并在最终报告中给出成功文件数量、失败文件数量、有效记录数量以及被拒绝的记录数量，提供明确的退出状态和可复现的命令示例。其次新增一个可查询的共享索引，以规范化的记录标识关联输入来源、验证结果和汇总输出，同一条原始记录只保留一个权威内容，其他报表通过索引引用它，避免为了多个视图重复复制完整数据；索引应允许按日期范围、类别和处理状态进行分页查询，明确处理同一文件的多次导入、源文件被修改、索引条目失效以及程序在写入过程中被中断的情况，恢复时不得将未完成的批次标记为成功，也不得丢失已经确认提交的结果，所有可调参数统一放在有说明的配置文件中并验证其类型和取值范围。第三，在现有进程内提供一个只读的本地查询接口，用于列出处理批次、查看单个批次的摘要并按页读取错误报告；该接口不提供上传、修改、删除或重新运行任务的入口，必须沿用应用现有的访问校验边界，不在示例、日志或测试输出中包含真实凭据和用户数据，路径输入必须限定在用户明确选择的目录内；命令行界面与查询接口应调用相同的领域服务，避免两套重复的数据验证与汇总实现。交付时请给出简洁的设计说明、模块职责、配置示例、安装与运行命令，以及覆盖正常文件、空目录、错误字段、重复导入、只读查询、分页边界和写入中断恢复的测试；确认在所有失败路径上都保持输入文件不变，未完成批次不会出现在成功结果中，并且读取索引和报告不会触发任何写操作；仅实现上述职责，不添加外部云服务、自动发布、账户注册或与数据整理无关的功能。";

#[tokio::test]
#[ignore = "requires explicit synthetic live prompt acceptance authorization"]
async fn synthetic_english_stage_response_metadata() {
    assert_eq!(
        std::env::var("AAG_PROMPT_ACCEPTANCE").as_deref(),
        Ok("REPRODUCE")
    );
    assert!(std::env::var_os("AAG_CONFIG_DIR").is_none());
    assert!(std::env::var_os("AAG_HOME_DIR").is_none());
    let model = std::env::var("AAG_PROMPT_MODEL").unwrap();
    let provider_name = std::env::var("AAG_PROMPT_PROVIDER_NAME").unwrap();
    let budget: u32 = std::env::var("AAG_PROMPT_BUDGET").unwrap().parse().unwrap();
    assert!((MIN_TOKENS..=MAX_TOKENS_LIMIT).contains(&budget));
    let db = Database::init().unwrap();
    let provider = db
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|provider| {
            provider.enabled && provider.name == provider_name && provider.models.contains(&model)
        })
        .expect("the configured provider and model must still exist");
    let target = target::Prompt_target::Provider {
        provider_id: provider.id,
        model: model.clone(),
    };
    let brief = if std::env::var("AAG_PROMPT_LONG_BRIEF").as_deref() == Ok("1") {
        SYNTHETIC_LONG_BRIEF
    } else {
        SYNTHETIC_BRIEF
    };
    let request = ChatCompletionRequest {
        model,
        messages: vec![
            message(
                "system",
                templates::effective_system_prompt(&db, templates::STAGE_EN_OPTIMIZE).unwrap(),
            ),
            message("user", compose_user_message(brief, None)),
        ],
        stream: false,
        temperature: Some(DEFAULT_TEMPERATURE),
        max_tokens: Some(budget),
        tools: None,
        tool_choice: None,
        extra: Default::default(),
        source_anthropic_request: None,
        source_gemini_request: None,
        source_responses_request: None,
        request_id: None,
        client_credential: None,
    };
    let dispatch = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        target::dispatch(&db, None, &target, &request),
    )
    .await
    .unwrap()
    .unwrap();
    let choice = dispatch.response.choices.first().unwrap();
    let thought_chars: usize = choice
        .message
        .extra
        .get("content_blocks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| block.get("thinking").and_then(Value::as_str))
        .map(|text| text.chars().count())
        .sum();
    let complete = choice.finish_reason.as_deref() == Some("stop")
        && !choice.message.content.as_deref().unwrap_or_default().trim().is_empty();
    println!(
        "{}",
        json!({"input_chars":brief.chars().count(),"budget":budget,"finish_reason":choice.finish_reason,"visible_chars":choice.message.content.as_deref().unwrap_or_default().chars().count(),"thinking_chars":thought_chars,"completion_tokens":dispatch.response.usage.as_ref().map(|usage|usage.completion_tokens),"result":if complete { "complete" } else { "incomplete" }})
    );
    assert!(
        complete,
        "synthetic English stage did not produce a final answer"
    );
}

#[tokio::test]
#[ignore = "requires explicit synthetic live prompt acceptance authorization"]
async fn synthetic_english_stage_completes_with_default_controls() {
    assert_eq!(
        std::env::var("AAG_PROMPT_ACCEPTANCE").as_deref(),
        Ok("VERIFY")
    );
    assert!(std::env::var_os("AAG_CONFIG_DIR").is_none());
    assert!(std::env::var_os("AAG_HOME_DIR").is_none());
    let model = std::env::var("AAG_PROMPT_MODEL").unwrap();
    let provider_name = std::env::var("AAG_PROMPT_PROVIDER_NAME").unwrap();
    let db = Database::init().unwrap();
    let provider = db
        .get_all_proxy_providers()
        .unwrap()
        .into_iter()
        .find(|provider| {
            provider.enabled && provider.name == provider_name && provider.models.contains(&model)
        })
        .expect("the selected configured provider and model must still exist");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        run_stage(
            &db,
            None,
            Prompt_stage_request {
                stage: templates::STAGE_EN_OPTIMIZE.into(),
                text: SYNTHETIC_LONG_BRIEF.into(),
                target: target::Prompt_target::Provider {
                    provider_id: provider.id,
                    model,
                },
                instruction: None,
                temperature: None,
                max_tokens: None,
                workspace_context: None,
            },
        ),
    )
    .await
    .unwrap();
    println!(
        "{}",
        match &result {
            Ok(output) =>
                json!({"input_chars":SYNTHETIC_LONG_BRIEF.chars().count(),"default_budget":DEFAULT_EN_MAX_TOKENS,
            "visible_chars":output.output.chars().count(),"elapsed_ms":output.elapsed_ms,
            "completion_tokens":output.usage.as_ref().map(|usage|usage.completion_tokens),"complete":output.complete,"failure_code":output.failure_code}),
            Err(code) => json!({"result":code}),
        }
    );
    assert!(
        result.as_ref().is_ok_and(|output| output.complete),
        "synthetic English stage did not produce a complete final answer"
    );
}
