import type { DiskItem } from "../types";

/** 可放心删、且没在使用的叶子项合计。 */
export function sumSafe(items: DiskItem[]): number {
  let n = 0;
  for (const it of items) {
    if (it.children.length) n += sumSafe(it.children);
    else if (it.safety === "safe" && !it.inUse) n += it.sizeBytes;
  }
  return n;
}
