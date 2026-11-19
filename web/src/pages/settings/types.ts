export interface SettingsDraft {
  [key: string]: unknown
  server: { bind: string; control_port: number }
  tenant: { default: string }
  ports: { start: number; end: number }
  http: { enabled: boolean; domain: string; bind: string; https_bind: string }
  acme: {
    enabled: boolean
    email: string
    staging: boolean
    challenge: string
    dns_provider: string
    dns_api_token: string
  }
  socks5: { enabled: boolean; bind: string }
  log: { level: string; format: string }
}

export interface SettingsNavItem {
  key: string
  href: string
  title: string
  label?: string
}

export interface SelectOption {
  value: string
  label: string
  description?: string
}
