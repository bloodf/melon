// Melon product mark. Export name stays FishLogo for upstream merge.

import type { IconProps } from './icons/props.ts'

/**
 * Render the Melon mark.
 * @param props.size - width in px (default 24; height matches width).
 * @param props.className - extra class for layout placement.
 * @returns the logo svg (aria-hidden; pair with the wordmark for accessibility).
 */
export function FishLogo({ size = 24, className }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
    >
      <rect x="2" y="2" width="20" height="20" rx="6" fill="currentColor" />
      <path d="M8 16V8h3.2c1.9 0 3.1 1.1 3.1 2.7 0 1.7-1.2 2.8-3.1 2.8H10.2V16H8zm2.2-4.2h.9c.8 0 1.3-.4 1.3-1.1 0-.7-.5-1.1-1.3-1.1h-.9v2.2z" fill="var(--dsw-alias-label-primary-inverted, #10130a)" />
    </svg>
  )
}
