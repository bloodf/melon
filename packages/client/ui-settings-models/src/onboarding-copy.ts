/** Durable settings namespace for product-wide GUI onboarding facts. */
export const WELCOME_NOTICE_SETTINGS_NAMESPACE = 'ui-onboarding'

/** Field storing the last welcome notice version the user acknowledged. */
export const WELCOME_NOTICE_ACK_FIELD = 'welcomeNoticeVersion'

/**
 * Bump only when the notice changes materially and every user should see it
 * again. The acknowledgement is compared for exact equality.
 */
export const WELCOME_NOTICE_VERSION = '2026-08-17.1'

export const WELCOME_NOTICE_COPY = {
  zh: {
    title: '欢迎使用 Melon',
    body: 'Melon 是 DeepSeek Harness 的桌面发行版。首次启动请安装本机 DurinDoor，或连接已有 DurinDoor。智能体循环、会话与工具仍由 DeepSeek Harness 提供。\n\nBuilt on DeepSeek Harness。',
    continueLabel: '继续',
  },
  en: {
    title: 'Welcome to Melon',
    body: 'Melon is the desktop distribution of DeepSeek Harness. First run, install DurinDoor locally or connect an existing DurinDoor. The agent loop, sessions, and tools remain DeepSeek Harness.\n\nBuilt on DeepSeek Harness.',
    continueLabel: 'Continue',
  },
} as const
