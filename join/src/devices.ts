// Noms des nœuds PipeWire créés par le binaire Rust `fdm` (il doit tourner AVANT).
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";

export const MEETING_SINK = "fdm_meeting"; // haut-parleur du bot : il ENTEND la réunion
export const AI_MIC = "fdm_ai_mic"; // micro du bot : il PARLE dans la réunion

function pactl(...args: string[]): string {
  return execFileSync("pactl", args, { encoding: "utf8" });
}

export function devicesPresent(): boolean {
  return pactl("list", "short", "sinks").includes(MEETING_SINK) && pactl("list", "short", "sources").includes(AI_MIC);
}

/** PIDs du navigateur (repéré par son profil) et de tous ses sous-processus : le son de Chromium
 * passe par un processus « audio service » enfant, sans le profil dans sa ligne de commande. */
export function browserPids(profileDir: string): Set<number> {
  const parent = new Map<number, number>();
  const pids = new Set<number>();
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      const cmd = readFileSync(`/proc/${name}/cmdline`, "utf8").replaceAll("\0", " ");
      const stat = readFileSync(`/proc/${name}/stat`, "utf8");
      const pid = Number(name);
      parent.set(pid, Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]));
      if (cmd.includes(profileDir)) pids.add(pid);
    } catch {
      /* processus disparu entre-temps */
    }
  }
  for (let changed = true; changed; ) {
    changed = false;
    for (const [pid, ppid] of parent) {
      if (pids.has(ppid) && !pids.has(pid)) {
        pids.add(pid);
        changed = true;
      }
    }
  }
  return pids;
}

type PaObject = { index: number; name?: string; sink?: number; source?: number; properties?: Record<string, string> };

function pactlJson(kind: string): PaObject[] {
  try {
    return JSON.parse(pactl("-f", "json", "list", kind) || "[]");
  } catch {
    return [];
  }
}

/** Filet de sécurité : si Chromium ignore PULSE_SINK / PULSE_SOURCE, on déplace ses flux. */
export function routeBrowserStreams(profileDir: string, log: (m: string) => void): void {
  const pids = browserPids(profileDir);
  if (pids.size === 0) return;
  const sinks = new Map(pactlJson("sinks").map((s) => [s.index, s.name]));
  const sources = new Map(pactlJson("sources").map((s) => [s.index, s.name]));
  const pidOf = (o: PaObject) => Number(o.properties?.["application.process.id"] ?? -1);
  for (const si of pactlJson("sink-inputs")) {
    if (pids.has(pidOf(si)) && sinks.get(si.sink!) !== MEETING_SINK) {
      pactl("move-sink-input", String(si.index), MEETING_SINK);
      log(`son du navigateur déplacé vers ${MEETING_SINK}`);
    }
  }
  for (const so of pactlJson("source-outputs")) {
    if (pids.has(pidOf(so)) && sources.get(so.source!) !== AI_MIC) {
      pactl("move-source-output", String(so.index), AI_MIC);
      log(`micro du navigateur déplacé vers ${AI_MIC}`);
    }
  }
}
