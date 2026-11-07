export type Platform = "teams" | "meet" | "zoom" | "unknown";

export function platformOf(url: string): Platform {
  let host: string;
  try {
    host = new URL(url).hostname;
  } catch {
    return "unknown";
  }
  if (/(^|\.)teams\.(microsoft|live)\.com$/.test(host)) return "teams";
  if (host === "meet.google.com") return "meet";
  if (/(^|\.)zoom\.us$/.test(host)) return "zoom";
  return "unknown";
}
