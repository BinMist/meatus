export interface NavItem {
  id: string
  label: string
  icon: string
}

export interface NavGroup {
  label: string
  items: NavItem[]
}

export type SessionMode = 'loopback' | 'session'
export type AdminRole = 'owner' | 'operator' | 'viewer' | 'auditor'
