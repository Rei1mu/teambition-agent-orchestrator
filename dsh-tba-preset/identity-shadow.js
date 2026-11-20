/**
 * 产品化说明：三模式共享的 identity 屏蔽插件（tba / tbaptc / chat）。
 * 实现放在 dsh-tba-preset，chat 复用同一份，避免重复。
 *
 * 在 preset scope 注册同名 harness:identity section，覆盖 global 的
 * "You are an AI agent powered by DeepSeek Harness."（空文本在 render 时被跳过）。
 * 与 dsh-persona shadow global persona 同一机制：不同 scope 同名不冲突。
 */
export const name = "identity-shadow"
export const inject = ["systemPrompt"]

export function apply(ctx) {
  ctx.effect(() => ctx.systemPrompt.section({
    name: "harness:identity",
    order: ctx.systemPrompt.getSectionOrder("HARNESS_IDENTITY"),
    text: "",
  }), "identity-shadow.section()")
}
