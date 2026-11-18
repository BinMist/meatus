import type { ProxyRow } from '../../types'

export interface ProxyDraft {
  [key: string]: unknown
  run_id: string
  name: string
  type: string
  local_addr: string
  remote_port: number
  domainsText: string
  max_conns: number
  idle_timeout_s: number
  rate_limit_bps: number
  allow_cidrs: string
}

export type ProxyRowView = ProxyRow & {
  [key: string]: unknown
  key: string
  endpoint: string
  local_addr: string
  domainsText: string
  traffic: string
}

export interface ProxyTypeOption {
  value: string
  label: string
  description: string
}
