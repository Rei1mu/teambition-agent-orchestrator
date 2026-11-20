import { isAbsolute, resolve } from "node:path";
import z from "@deepseek-ai/schemastery";
import { TOOL_ABORTED, ToolArgsError, ToolContextUnavailableError, defineTool } from "@deepseek-ai/dsh-tools";
import { HarnessError } from "@deepseek-ai/dsh-llm";
import { JobError } from "@deepseek-ai/dsh-jobs";
import { ESCALATION_TARGETS, SandboxEscalationError, approveEscalation, escalationHintMarker, sandboxDenialMarker, sandboxPermissionsDescription, validateEscalationArgs } from "@deepseek-ai/dsh-sandbox";
import { subprocessSpawnFailure } from "@deepseek-ai/dsh-subprocess";
import { parseExitStatus } from "@deepseek-ai/dsh-shell";

const name = "pwsh-lite";
const inject = ["tools", "shell", "systemPrompt", "shellEnv", "workingDirectory"];
const Config = z.object({
  enableRunInBackground: z.boolean().default(true),
  promoteOnTimeout: z.boolean().default(true),
});

const DESCRIPTION = "pwsh -Command→stdout/err;new proc;workdir≠cd;Win paths,$env:NAME;description first;exit≠0:[exit code:N];Win kill:[exit code:1]=abort;bg→job(job_output/job_kill);sandbox_permissions+justification→wider";

function validateArgs(args) {
  if (args.command.trim().length === 0) throw new ToolArgsError(["invalid command: expected a non-empty string"]);
  if (args.description.trim().length === 0) throw new ToolArgsError(["invalid description: expected a non-empty string"]);
  if (args.timeoutMs !== undefined && (!Number.isFinite(args.timeoutMs) || args.timeoutMs <= 0)) {
    throw new ToolArgsError([`invalid timeoutMs: expected a positive number, got ${JSON.stringify(args.timeoutMs)}`]);
  }
  validateEscalationArgs(args.sandbox_permissions, args.justification);
}

function sandboxDenialNotes(sandbox, escalationModes) {
  if (sandbox?.runnerFailed) return [`[sandbox: the sandbox runner itself failed under ${sandbox.mode} mode — the command did not run; this is a sandbox problem, not a command failure]`];
  if (sandbox?.denied) {
    const notes = [sandboxDenialMarker(sandbox.mode)];
    if (escalationModes.length > 0) notes.push(escalationHintMarker("command"));
    return notes;
  }
  return [];
}

function processOutcome(proc, escalationModes = []) {
  const base = proc.status === "killed"
    ? { status: "killed", detail: proc.signal !== null ? `signal: ${proc.signal}` : "killed before exit" }
    : { status: "completed", detail: `exit code: ${proc.exitCode ?? 0}` };
  const notes = sandboxDenialNotes(proc.sandbox, escalationModes);
  return notes.length === 0 ? base : { ...base, detail: `${base.detail}; ${notes.join(" ")}` };
}

function processSources(proc) {
  const source = (channel) => ({
    channel,
    read: (fromByte) => {
      const live = proc();
      return live === undefined ? { text: "", nextOffset: fromByte, lossy: false } : live.observed[channel].readFrom(fromByte);
    },
  });
  return [source("stdout"), source("stderr")];
}

function ringDelta(chunks) {
  const out = chunks.filter((chunk) => chunk.channel !== "stderr").map((chunk) => chunk.text).join("");
  const err = chunks.filter((chunk) => chunk.channel === "stderr").map((chunk) => chunk.text).join("");
  const separator = out.length > 0 && !out.endsWith("\n") ? "\n" : "";
  return out + (err.length > 0 ? `${separator}[stderr]\n${err}` : "");
}

function processJob(start, outcome) {
  const controller = new AbortController();
  let process;
  let failure;
  return {
    cancel: (reason) => {
      if (controller.signal.aborted) return;
      controller.abort(reason);
      process?.kill();
    },
    done: (async () => {
      try {
        process = await start(controller.signal);
        try {
          if (controller.signal.aborted) process.kill();
        } finally {
          await process.done;
        }
        return outcome(process);
      } catch (error) {
        const killed = controller.signal.aborted && process === undefined;
        if (!killed) failure = { error };
        return {
          status: killed ? "killed" : "failed",
          detail: error instanceof Error ? error.message : String(error),
        };
      }
    })(),
    failure: () => failure,
  };
}

function streamText(output) {
  if (!output.truncated) return output.text;
  return `${output.text}\n[output truncated; full output: ${output.spillPath ?? "(unavailable)"}]`;
}

function renderResult(result, escalationModes = []) {
  const out = streamText(result.stdout);
  const err = streamText(result.stderr);
  let body = out;
  if (err.length > 0) {
    if (body.length > 0 && !body.endsWith("\n")) body += "\n";
    body += `[stderr]\n${err}`;
  }
  if (body.length === 0) body = "(no output)";
  const markers = [];
  if (result.sandbox?.runnerFailed) markers.push(`[sandbox: the sandbox runner itself failed under ${result.sandbox.mode} mode — the command did not run; this is a sandbox problem, not a command failure]`);
  if (result.sandbox?.denied) {
    markers.push(sandboxDenialMarker(result.sandbox.mode));
    if (escalationModes.length > 0) markers.push(escalationHintMarker("command"));
  }
  if (result.timedOut) markers.push(`[timed out after ${result.timeoutMs}ms]`);
  if (result.stopped !== undefined) markers.push(`[stopped: ${result.stopped}]`);
  if (result.signal !== null) markers.push(`[killed by signal: ${result.signal}]`);
  else if (result.exitCode !== 0) markers.push(`[exit code: ${result.exitCode}]`);
  if (markers.length === 0) return body;
  if (!body.endsWith("\n")) body += "\n";
  return body + markers.join("\n");
}

function renderPromoted(promoted) {
  const body = promoted.output.length > 0
    ? promoted.output.endsWith("\n") ? promoted.output : `${promoted.output}\n`
    : "";
  return `${body}[still running after ${promoted.timeoutMs}ms; moved to background job ${promoted.jobId}]`;
}

function renderJobRead(delta, lossy, spillPaths, sandbox, escalationModes = []) {
  const notices = [];
  if (lossy) notices.push(`[some output was dropped from memory; full output: ${spillPaths.length > 0 ? spillPaths.join(", ") : "(unavailable)"}]`);
  if (sandbox?.runnerFailed) notices.push(`[sandbox: the sandbox runner itself failed under ${sandbox.mode} mode — the command did not run; this is a sandbox problem, not a command failure]`);
  else if (sandbox?.denied) {
    notices.push(sandboxDenialMarker(sandbox.mode));
    if (escalationModes.length > 0) notices.push(escalationHintMarker("command"));
  }
  if (notices.length === 0) return delta;
  return `${delta}${delta.length > 0 && !delta.endsWith("\n") ? "\n" : ""}${notices.join("\n")}`;
}

function resolveWorkdir(modelWorkdir, sessionCwd) {
  if (modelWorkdir === undefined) return sessionCwd;
  if (sessionCwd !== undefined && !isAbsolute(modelWorkdir)) return resolve(sessionCwd, modelWorkdir);
  return modelWorkdir;
}

function canonicalResult(result, cwd) {
  const output = (stream) => ({
    text: stream.text,
    truncated: stream.truncated,
    ...stream.spillPath !== undefined ? { spillPath: stream.spillPath } : {},
  });
  return {
    cwd,
    kind: "foreground",
    exitCode: result.exitCode,
    signal: result.signal,
    timedOut: result.timedOut,
    aborted: result.aborted,
    timeoutMs: result.timeoutMs,
    stdout: output(result.stdout),
    stderr: output(result.stderr),
    ...result.sandbox !== undefined ? { sandbox: {
      mode: result.sandbox.mode,
      denied: result.sandbox.denied,
      ...result.sandbox.enforcement !== undefined ? { enforcement: result.sandbox.enforcement } : {},
      ...result.sandbox.runnerFailed !== undefined ? { runnerFailed: result.sandbox.runnerFailed } : {},
    } } : {},
  };
}

function toolAborted() {
  const error = new HarnessError("tool call aborted", TOOL_ABORTED);
  error.name = "AbortError";
  return error;
}

function launchFailure(error, signal) {
  return signal.aborted && error === signal.reason ? error : subprocessSpawnFailure(error, "pwsh-lite");
}

const BACKGROUND_PROPERTIES = {
  kind: { type: "string", required: true, const: "background" },
  jobId: { type: "string", required: true },
  cwd: { type: "string", required: true },
};

function apply(ctx, config = {}) {
  const backgroundEnabled = config.enableRunInBackground ?? true;
  const promoteOnTimeout = (config.promoteOnTimeout ?? true) && backgroundEnabled;
  const defaultMode = ctx.shell.sandboxMode;
  const escalationModes = defaultMode === undefined ? [] : ESCALATION_TARGETS;
  const sandboxPolicy = defaultMode === undefined ? undefined : ctx.get("sandboxPolicy");
  if (defaultMode !== undefined && sandboxPolicy === undefined) {
    throw new Error("pwsh-lite: the mounted shell executor confines but ctx.sandboxPolicy is missing");
  }

  const resolveSandboxPolicy = (exec) => sandboxPolicy?.resolve(exec.agent === undefined ? {} : { session: exec.agent.session });
  const approveEscalationRequest = (mode, justification, exec, standingPolicy) => {
    if (escalationModes.length === 0) {
      throw new SandboxEscalationError("sandbox_permissions is not available in this composition (no sandboxing executor to escalate)", "SANDBOX_ESCALATION_UNAVAILABLE");
    }
    return approveEscalation({
      requestedMode: mode,
      justification,
      effectiveMode: standingPolicy.mode,
      subject: "command",
    }, {
      approver: ctx.get("approval"),
      agent: exec.agent,
      callId: exec.callId,
      toolName: "ps",
      signal: exec.signal,
    });
  };

  const tool = (jobs) => {
    const background = jobs !== undefined;
    const promote = background && promoteOnTimeout;

    const startJob = (registry, args, exec, spec) => {
      let proc;
      let stopped;
      let hooks;
      return {
        id: registry.start({
          kind: "pwsh",
          label: args.command,
          toolCall: { callId: exec.callId, rootCallId: exec.rootCallId },
          ...exec.agent ? { owner: exec.agent.id } : {},
          output: processSources(() => proc),
          run: () => {
            const job = processJob(async (signal) => {
              proc = await ctx.shell.execute({ ...spec, signal });
              return proc;
            }, (started) => processOutcome(started, escalationModes));
            hooks = job;
            return {
              done: job.done,
              cancel: (reason) => {
                stopped = reason;
                job.cancel(reason);
              },
            };
          },
        }),
        process: () => proc,
        stopped: () => stopped,
        failure: () => hooks?.failure(),
      };
    };

    const waitOnJob = async (registry, attached, exec, spec) => {
      const owner = exec.agent?.id;
      const timeoutMs = spec.timeoutMs;
      const stop = async (reason) => {
        registry.kill(attached.id, owner, reason);
        const settled = await registry.wait(attached.id, timeoutMs, owner);
        if (settled.status !== "running" && settled.status !== "stopping") registry.remove(attached.id, owner);
        return settled;
      };
      let view;
      try {
        view = await registry.wait(attached.id, timeoutMs, owner, exec.signal);
      } catch {
        await stop("tool call aborted");
        throw toolAborted();
      }
      if ((view.status === "running" || view.status === "stopping") && attached.process() === undefined) {
        await stop("timed out during preparation");
        return {
          kind: "foreground",
          cwd: spec.workdir,
          exitCode: null,
          signal: null,
          timedOut: true,
          aborted: false,
          timeoutMs,
          stdout: { text: "", truncated: false },
          stderr: { text: "", truncated: false },
          ...spec.sandboxPolicy !== undefined ? { sandbox: { mode: spec.sandboxPolicy.mode, denied: false } } : {},
        };
      }
      if (view.status === "running" || view.status === "stopping") {
        const read = registry.read(attached.id, owner);
        return {
          kind: "promoted",
          cwd: spec.workdir,
          jobId: attached.id,
          timeoutMs,
          output: renderJobRead(ringDelta(read.chunks), read.lossy, read.job.output.spillPaths ?? [], attached.process()?.sandbox, escalationModes),
        };
      }
      registry.remove(attached.id, owner);
      const process = attached.process();
      if (process === undefined) {
        const failure = attached.failure();
        throw failure === undefined ? new Error(view.detail) : launchFailure(failure.error, exec.signal);
      }
      const result = await process.result().catch((error) => {
        throw launchFailure(error, exec.signal);
      });
      const stopped = attached.stopped();
      return { ...canonicalResult(result, spec.workdir), ...stopped !== undefined ? { stopped } : {} };
    };

    return defineTool({
      name: "ps",
      description: DESCRIPTION,
      parameters: {
        description: {
          type: "string",
          required: true,
          description: "Clear, concise description of what this command does in active voice, 5-10 words (shown in the UI).",
        },
        command: { type: "string", required: true, description: "The PowerShell command to execute." },
        timeoutMs: {
          type: "number",
          description: promote ? "Timeout in milliseconds. On expiry the command moves to the background as a job." : "Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry.",
        },
        workdir: {
          type: "string",
          description: "Working directory for this command. Defaults to the session workspace; a relative path is resolved against it.",
        },
        ...background ? { run_in_background: {
          type: "boolean",
          description: "Run in the background and return a job id immediately (collect with job_output, stop with job_kill). No timeout applies.",
        } } : {},
        ...escalationModes.length > 0 ? {
          sandbox_permissions: { type: "string", enum: [...escalationModes], description: sandboxPermissionsDescription("command") },
          justification: { type: "string", description: "Required with sandbox_permissions: one sentence for the user explaining why this exact command needs the wider access." },
        } : {},
      },
      output: {
        schema: { oneOf: [
          { type: "object", additionalProperties: false, properties: BACKGROUND_PROPERTIES },
          { type: "object", additionalProperties: false, properties: {
            kind: { type: "string", required: true, const: "promoted" },
            cwd: { type: "string", required: true },
            jobId: { type: "string", required: true },
            timeoutMs: { type: "number", required: true },
            output: { type: "string", required: true },
          } },
          { type: "object", additionalProperties: false, properties: {
            kind: { type: "string", required: true, const: "foreground" },
            cwd: { type: "string", required: true },
            exitCode: { required: true, oneOf: [{ type: "integer" }, { type: "null" }] },
            signal: { required: true, oneOf: [{ type: "string" }, { type: "null" }] },
            timedOut: { type: "boolean", required: true },
            aborted: { type: "boolean", required: true },
            stopped: { type: "string" },
            timeoutMs: { type: "number", required: true },
            stdout: { type: "object", additionalProperties: false, required: true, properties: {
              text: { type: "string", required: true },
              truncated: { type: "boolean", required: true },
              spillPath: { type: "string" },
            } },
            stderr: { type: "object", additionalProperties: false, required: true, properties: {
              text: { type: "string", required: true },
              truncated: { type: "boolean", required: true },
              spillPath: { type: "string" },
            } },
            sandbox: { type: "object", additionalProperties: false, properties: {
              mode: { type: "string", required: true },
              denied: { type: "boolean", required: true },
              enforcement: { type: "string" },
              runnerFailed: { type: "boolean" },
            } },
          } },
        ] },
        render: (_args, value) => [{
          type: "text",
          text: value.kind === "background"
            ? `started background job ${value.jobId}`
            : value.kind === "promoted" ? renderPromoted(value) : renderResult(value, escalationModes),
        }],
        presentationMeta: (_args, value) => ({ cwd: value.cwd }),
      },
      async execute(args, exec) {
        validateArgs(args);
        const standingPolicy = resolveSandboxPolicy(exec);
        const approvedMode = args.sandbox_permissions !== undefined && args.justification !== undefined
          ? await approveEscalationRequest(args.sandbox_permissions, args.justification, exec, standingPolicy)
          : undefined;
        const policy = approvedMode === undefined ? standingPolicy : { ...standingPolicy, mode: approvedMode };
        const cwd = exec.agent === undefined ? undefined : await ctx.workingDirectory.ensure(exec.agent, exec.signal);
        const workdir = resolveWorkdir(args.workdir, cwd);
        const request = {
          command: args.command,
          ...workdir !== undefined ? { workdir } : {},
          ...args.timeoutMs !== undefined ? { timeoutMs: args.timeoutMs } : {},
          dshEnv: ctx.shellEnv.collect(exec),
          ...policy !== undefined ? { sandboxPolicy: policy } : {},
        };
        if (args.run_in_background === true) {
          if (!backgroundEnabled) throw new JobError("run_in_background is disabled for this deployment (enableRunInBackground: false)", "JOB_BACKGROUND_DISABLED");
          if (jobs === undefined) throw new ToolContextUnavailableError("background jobs unavailable: load @deepseek-ai/dsh-jobs and @deepseek-ai/dsh-tool-jobs");
          if (exec.signal.aborted) throw toolAborted();
          const spec = ctx.shell.resolve({ ...request, onExpiry: "none" });
          return { kind: "background", jobId: startJob(jobs, args, exec, spec).id, cwd: spec.workdir };
        }
        if (jobs !== undefined && promote) {
          const spec = ctx.shell.resolve({ ...request, onExpiry: "none" });
          let attached;
          try {
            attached = startJob(jobs, args, exec, spec);
          } catch (error) {
            ctx.logger.warn(`pwsh-lite: job registration refused, running in the foreground with the timeout kill instead: ${String(error)}`);
          }
          if (attached !== undefined) return waitOnJob(jobs, attached, exec, spec);
        }
        const spec = ctx.shell.resolve({ ...request, signal: exec.signal });
        let result;
        try {
          result = await (await ctx.shell.execute(spec)).result();
        } catch (error) {
          throw launchFailure(error, exec.signal);
        }
        if (result.aborted) throw toolAborted();
        return canonicalResult(result, spec.workdir);
      },
      presentCall: (args) => {
        if (args.run_in_background === true) return {
          card: "generic",
          title: args.command,
          kind: "execute",
          rawInput: args.command,
          content: [{ type: "text", text: args.description }],
        };
        return {
          card: "terminal",
          title: args.command,
          description: args.description,
          ...args.workdir !== undefined ? { cwd: args.workdir } : {},
        };
      },
      presentResult: (args, result) => {
        const block = result.content.length === 1 ? result.content[0] : undefined;
        if (block === undefined || block.type !== "text") return undefined;
        const raw = block.text;
        const isBackground = typeof args === "object" && args !== null && args.run_in_background === true;
        const isPromoted = result.value?.kind === "promoted";
        if (isBackground || isPromoted || result.isError) {
          return { card: "generic", content: [{ type: "text", text: `\`\`\`console\n${raw.replace(/\n+$/, "")}\n\`\`\`` }] };
        }
        const { body, ...exit } = parseExitStatus(raw);
        return { card: "terminal", output: body, ...exit };
      },
    });
  };

  if (!backgroundEnabled) {
    ctx.tools.register(tool(undefined));
    return;
  }
  let foregroundOnly = ctx.get("jobs") === undefined ? ctx.tools.register(tool(undefined)) : undefined;
  ctx.inject(["jobs"], (jobCtx) => {
    foregroundOnly?.();
    foregroundOnly = undefined;
    const unregister = ctx.tools.register(tool(jobCtx.jobs));
    jobCtx.effect(() => () => {
      unregister();
      if (ctx.fiber.state === 2) foregroundOnly = ctx.tools.register(tool(undefined));
    });
  });
}

export { Config, apply, inject, name };
