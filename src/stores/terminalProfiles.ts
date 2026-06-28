import { createStore } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";

export type Platform = "windows" | "linux" | "macos";

export interface TerminalProfile {
  id: string;
  name: string;
  program: string;
  args: string;
  defaultWorkingDirectory: string;
  platform: Platform;
}

interface TerminalProfilesState {
  profiles: TerminalProfile[];
  defaultProfileId: string | null;
}

interface LegacyTerminalProfile {
  id: string;
  name: string;
  program: string;
  args: string;
  source?: "builtin" | "custom";
  platform: Platform;
}

function detectPlatform(): Platform {
  const ua = navigator.userAgent;
  if (ua.includes("Windows")) return "windows";
  if (ua.includes("Macintosh") || ua.includes("Mac OS")) return "macos";
  return "linux";
}

async function checkProgram(path: string): Promise<boolean> {
  try {
    return await invoke<boolean>("check_program", { path });
  } catch {
    return false;
  }
}

export let detectedPlatform: Platform = "linux";

function storageKey(): string {
  return `xxterm.terminalProfiles.${detectedPlatform}.v1`;
}

const windowsSeedProfiles: TerminalProfile[] = [
  { id: "windows:powershell", name: "PowerShell", program: "powershell.exe", args: "", defaultWorkingDirectory: "", platform: "windows" },
  { id: "windows:cmd", name: "Command Prompt", program: "cmd.exe", args: "", defaultWorkingDirectory: "", platform: "windows" },
  { id: "windows:wsl", name: "WSL", program: "wsl.exe", args: "", defaultWorkingDirectory: "", platform: "windows" },
];

const unixSeedCandidates: { id: string; name: string; program: string }[] = [
  { id: "unix:bash", name: "Bash", program: "/bin/bash" },
  { id: "unix:zsh", name: "Zsh", program: "/bin/zsh" },
  { id: "unix:fish", name: "Fish", program: "/usr/bin/fish" },
];

let linuxSeedProfiles: TerminalProfile[] = [
  { id: "unix:bash", name: "Bash", program: "/bin/bash", args: "", defaultWorkingDirectory: "", platform: "linux" },
  { id: "unix:zsh", name: "Zsh", program: "/bin/zsh", args: "", defaultWorkingDirectory: "", platform: "linux" },
  { id: "unix:fish", name: "Fish", program: "/usr/bin/fish", args: "", defaultWorkingDirectory: "", platform: "linux" },
];

let macosSeedProfiles: TerminalProfile[] = [
  { id: "unix:bash", name: "Bash", program: "/bin/bash", args: "", defaultWorkingDirectory: "", platform: "macos" },
  { id: "unix:zsh", name: "Zsh", program: "/bin/zsh", args: "", defaultWorkingDirectory: "", platform: "macos" },
  { id: "unix:fish", name: "Fish", program: "/usr/bin/fish", args: "", defaultWorkingDirectory: "", platform: "macos" },
];

export const [terminalProfiles, setTerminalProfiles] = createStore<TerminalProfilesState>({
  profiles: [],
  defaultProfileId: null,
});

function isTerminalProfile(value: unknown): value is TerminalProfile {
  if (!value || typeof value !== "object") return false;
  const profile = value as Partial<TerminalProfile>;
  return (
    typeof profile.id === "string" &&
    typeof profile.name === "string" &&
    typeof profile.program === "string" &&
    typeof profile.args === "string" &&
    typeof profile.defaultWorkingDirectory === "string" &&
    profile.platform === detectedPlatform
  );
}

function isLegacyTerminalProfile(value: unknown): value is LegacyTerminalProfile {
  if (!value || typeof value !== "object") return false;
  const profile = value as Partial<LegacyTerminalProfile>;
  return (
    typeof profile.id === "string" &&
    typeof profile.name === "string" &&
    typeof profile.program === "string" &&
    typeof profile.args === "string" &&
    profile.platform === detectedPlatform
  );
}

function platformSeedProfiles(): TerminalProfile[] {
  switch (detectedPlatform) {
    case "windows":
      return windowsSeedProfiles;
    case "macos":
      return macosSeedProfiles;
    default:
      return linuxSeedProfiles;
  }
}

function normalizeProfile(profile: TerminalProfile): TerminalProfile {
  return {
    id: profile.id,
    name: profile.name.trim(),
    program: profile.program.trim(),
    args: profile.args.trim(),
    defaultWorkingDirectory: profile.defaultWorkingDirectory.trim(),
    platform: detectedPlatform,
  };
}

function normalizeProfiles(profiles: TerminalProfile[]): TerminalProfile[] {
  const seen = new Set<string>();
  const normalized: TerminalProfile[] = [];
  for (const profile of profiles) {
    if (seen.has(profile.id)) continue;
    const next = normalizeProfile(profile);
    if (!next.name || !next.program) continue;
    seen.add(next.id);
    normalized.push(next);
  }
  return normalized;
}

function persistProfiles(profiles: TerminalProfile[], defaultProfileId: string | null) {
  const payload: TerminalProfilesState = { profiles, defaultProfileId };
  localStorage.setItem(storageKey(), JSON.stringify(payload));
}

function applyProfiles(profiles: TerminalProfile[], defaultProfileId: string | null) {
  const nextProfiles = normalizeProfiles(profiles);
  const nextDefaultId =
    (defaultProfileId && nextProfiles.some((profile) => profile.id === defaultProfileId) ? defaultProfileId : null) ??
    nextProfiles[0]?.id ??
    null;
  setTerminalProfiles({
    profiles: nextProfiles,
    defaultProfileId: nextDefaultId,
  });
  persistProfiles(nextProfiles, nextDefaultId);
}

function loadLegacyProfiles(raw: string | null): { profiles: TerminalProfile[]; defaultProfileId: string | null } | null {
  try {
    const parsed = JSON.parse(raw || "null");
    if (!Array.isArray(parsed)) return null;

    const customProfiles = parsed
      .filter(isLegacyTerminalProfile)
      .map((profile) =>
        normalizeProfile({
          id: profile.id,
          name: profile.name,
          program: profile.program,
          args: profile.args,
          defaultWorkingDirectory: "",
          platform: profile.platform,
        }),
      );

    const profiles = [...platformSeedProfiles(), ...customProfiles];
    return {
      profiles,
      defaultProfileId: profiles[0]?.id ?? null,
    };
  } catch {
    return null;
  }
}

export function loadTerminalProfiles() {
  const raw = localStorage.getItem(storageKey());

  try {
    const parsed = JSON.parse(raw || "null") as Partial<TerminalProfilesState> | null;
    if (parsed && Array.isArray(parsed.profiles)) {
      const profiles = parsed.profiles.filter(isTerminalProfile);
      applyProfiles(profiles.length > 0 ? profiles : platformSeedProfiles(), parsed.defaultProfileId ?? null);
      return;
    }
  } catch {
  }

  const legacy = loadLegacyProfiles(raw);
  if (legacy) {
    applyProfiles(legacy.profiles, legacy.defaultProfileId);
    return;
  }

  applyProfiles(platformSeedProfiles(), platformSeedProfiles()[0]?.id ?? null);
}

export function getDefaultTerminalProfile(): TerminalProfile {
  const defaultProfile = terminalProfiles.profiles.find((profile) => profile.id === terminalProfiles.defaultProfileId);
  return defaultProfile ?? terminalProfiles.profiles[0] ?? platformSeedProfiles()[0];
}

export function isDefaultTerminalProfile(id: string): boolean {
  return terminalProfiles.defaultProfileId === id;
}

export function getTerminalProfiles(): TerminalProfile[] {
  return terminalProfiles.profiles;
}

export function addTerminalProfile(input: { name: string; program: string; args: string; defaultWorkingDirectory: string }): TerminalProfile {
  const profile = normalizeProfile({
    id: `${detectedPlatform}:profile:${crypto.randomUUID()}`,
    name: input.name,
    program: input.program,
    args: input.args,
    defaultWorkingDirectory: input.defaultWorkingDirectory,
    platform: detectedPlatform,
  });
  applyProfiles([...terminalProfiles.profiles, profile], terminalProfiles.defaultProfileId);
  return profile;
}

export function updateTerminalProfile(
  id: string,
  input: { name: string; program: string; args: string; defaultWorkingDirectory: string },
): TerminalProfile | null {
  if (isDefaultTerminalProfile(id)) return null;
  const index = terminalProfiles.profiles.findIndex((profile) => profile.id === id);
  if (index === -1) return null;

  const nextProfiles = terminalProfiles.profiles.slice();
  nextProfiles[index] = normalizeProfile({
    ...nextProfiles[index],
    name: input.name,
    program: input.program,
    args: input.args,
    defaultWorkingDirectory: input.defaultWorkingDirectory,
  });
  applyProfiles(nextProfiles, terminalProfiles.defaultProfileId);
  return nextProfiles[index];
}

export function deleteTerminalProfile(id: string): boolean {
  if (isDefaultTerminalProfile(id)) return false;
  const nextProfiles = terminalProfiles.profiles.filter((profile) => profile.id !== id);
  if (nextProfiles.length === terminalProfiles.profiles.length) return false;
  applyProfiles(nextProfiles, terminalProfiles.defaultProfileId);
  return true;
}

export function setDefaultTerminalProfile(id: string): boolean {
  if (!terminalProfiles.profiles.some((profile) => profile.id === id)) return false;
  applyProfiles(terminalProfiles.profiles, id);
  return true;
}

export async function initPlatformProfiles() {
  detectedPlatform = detectPlatform();

  if (detectedPlatform !== "windows") {
    const available: TerminalProfile[] = [];
    for (const candidate of unixSeedCandidates) {
      const exists = await checkProgram(candidate.program);
      if (exists) {
        available.push({
          ...candidate,
          args: "",
          defaultWorkingDirectory: "",
          platform: detectedPlatform,
        });
      }
    }
    if (available.length === 0) {
      available.push({
        id: "unix:bash",
        name: "Bash",
        program: "/bin/bash",
        args: "",
        defaultWorkingDirectory: "",
        platform: detectedPlatform,
      });
    }
    if (detectedPlatform === "macos") {
      macosSeedProfiles = available;
    } else {
      linuxSeedProfiles = available;
    }
  }

  loadTerminalProfiles();
}
