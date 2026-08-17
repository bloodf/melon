// Melon product wordmark. Export name stays BrandWordmark for upstream merge.

import type { IconProps } from './icons/props.ts'

/**
 * Render the Melon wordmark.
 * @param props.size - height in px (default 24; width keeps a 120:24 ratio).
 * @param props.className - extra class for layout placement.
 * @returns the wordmark svg (aria-hidden decorative brand art).
 */
export function BrandWordmark({ size = 24, className }: IconProps) {
  return (
    <svg
      width={(size * 120) / 24}
      height={size}
      className={className}
      viewBox="0 0 120 24"
      fill="none"
      aria-hidden="true"
    >
      <rect x="0" y="2" width="20" height="20" rx="5" fill="currentColor" />
      <text x="5" y="17" fill="var(--dsw-alias-label-primary-inverted, #10130a)" fontFamily="ui-sans-serif, system-ui, sans-serif" fontSize="14" fontWeight="800">M</text>
      <text x="28" y="17" fill="currentColor" fontFamily="ui-sans-serif, system-ui, sans-serif" fontSize="16" fontWeight="800">Melon</text>
    </svg>
  )
}
