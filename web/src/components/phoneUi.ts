// Mock (prototype.html) class tokens for the phone UI. One home per value the
// mock reuses; sizes that differ per site stay at the site.
const IBI_SHAPE = "grid h-[34px] w-[34px] place-items-center rounded-[11px] border";
/** `.ib` — 44×44 hit area around an `.ibi` (proto:126). */
export const IB = "relative flex h-11 w-11 shrink-0 cursor-pointer items-center justify-center";
/** `.ibi` — 34px r11 surface, --s1 / --bd / --mut (proto:127). */
export const IBI = `${IBI_SHAPE} border-zinc-800 bg-zinc-900 text-zinc-400`;
/** `.ibi.hi` — --tx / --bd2 (proto:128). */
export const IBI_HI = `${IBI_SHAPE} border-zinc-700 bg-zinc-900 text-zinc-100`;
/** `.ibi.on` — emerald tint (proto:129). */
export const IBI_ON = `${IBI_SHAPE} border-emerald-600/45 bg-emerald-600/16 text-emerald-400`;
/** `.pill` / `.segpill` faces — colours only; size per site (proto:224-225, 283-284). */
export const PILL_OFF = "border-zinc-800 bg-zinc-950 font-medium text-zinc-400";
export const PILL_ON = "border-zinc-100 bg-zinc-100 font-semibold text-zinc-950";
/** `.mtag` — machine tag / lane / sub-agent tag (proto:81). */
export const MTAG = "shrink-0 rounded-[5px] bg-zinc-800 px-[5px] text-[10.5px] leading-4 text-zinc-400";
/** `.ph2` panel header (proto:230-231). */
export const PANEL_HEAD = "flex items-baseline justify-between px-0.5 pb-2 pt-1";
export const PANEL_TITLE = "text-[15px] font-medium text-zinc-100";
export const PANEL_SUB = "text-xs text-zinc-500";
/** `.trow` / `.krow` 52px list row (proto:234, 240). */
export const ROW = "flex min-h-[52px] items-center gap-3 border-b border-zinc-800 px-0.5 py-1.5 last:border-b-0";
/** `.kwatch` / `.kstop` action pill shape (proto:247, 249); colour per site. */
export const ACTION_PILL = "flex h-[34px] shrink-0 cursor-pointer items-center rounded-[17px] px-3.5 text-[13px] font-semibold";
