/**
 * 异步写入结束后按 store 真值回写 DOM 属性。
 *
 * Vue 3.5 在 patchElement 时只看 vnode 的 prop 值有没有变（checked 从旧值到新值）：
 * 写操作失败后 store 真值没变，整条 checked 补丁就被跳过，开关会停在用户点击后的
 * 视觉状态（与真相相反）。所以 await 的写操作结束后必须显式回写一次。
 */
export function syncChecked(el: { checked: boolean }, value: boolean): void {
  el.checked = value;
}
