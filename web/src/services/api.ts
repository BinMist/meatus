import { reactive } from 'vue'
import type { SessionSnapshot } from '../types'

export const session = reactive({
  ready: false,
  authenticated: false,
  mode: '' as '' | 'loopback' | 'session',
  csrf: '',
})

async function parseJson<T>(response: Response): Promise<T> {
  const body = (await response.json().catch(() => null)) as T | null
  if (!response.ok) {
    const message =
      body && typeof body === 'object' && 'error' in body
        ? String((body as { error: unknown }).error)
        : `HTTP ${response.status}`
    throw new Error(message)
  }
  if (!body) throw new Error('empty response')
  return body
}

export async function refreshSession(): Promise<SessionSnapshot> {
  const response = await fetch('/api/v1/session', {
    credentials: 'include',
    headers: { Accept: 'application/json' },
  })
  const snapshot = await response.json() as SessionSnapshot
  session.authenticated = snapshot.authenticated
  session.mode = snapshot.mode
  session.csrf = snapshot.csrf ?? ''
  session.ready = true
  return snapshot
}

export async function login(token: string): Promise<SessionSnapshot> {
  const response = await fetch('/api/v1/session', {
    method: 'POST',
    credentials: 'include',
    headers: {
      Accept: 'application/json',
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({ token }),
  })
  const snapshot = await parseJson<SessionSnapshot>(response)
  session.authenticated = snapshot.authenticated
  session.mode = snapshot.mode
  session.csrf = snapshot.csrf ?? ''
  session.ready = true
  return snapshot
}

export async function logout(): Promise<void> {
  const response = await fetch('/api/v1/session', {
    method: 'DELETE',
    credentials: 'include',
    headers: {
      Accept: 'application/json',
      'X-Meatus-CSRF': session.csrf,
    },
  })
  if (!response.ok && response.status !== 401) {
    await parseJson(response)
  }
  session.authenticated = false
  session.csrf = ''
}

export async function apiWrite<T>(
  method: 'POST' | 'PUT' | 'DELETE',
  path: string,
  body?: unknown,
): Promise<T> {
  const response = await fetch(path, {
    method,
    credentials: 'include',
    headers: {
      Accept: 'application/json',
      'Content-Type': 'application/json',
      'X-Meatus-CSRF': session.csrf,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 401) {
    session.authenticated = false
  }
  return parseJson<T>(response)
}

export async function apiGet<T>(path: string): Promise<T> {
  const response = await fetch(path, {
    credentials: 'include',
    headers: { Accept: 'application/json' },
  })
  if (response.status === 401) {
    session.authenticated = false
  }
  return parseJson<T>(response)
}

export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1)
  const scaled = value / 1024 ** index
  return `${scaled >= 100 ? scaled.toFixed(0) : scaled.toFixed(1)} ${units[index]}`
}

export function formatNumber(value: number): string {
  return new Intl.NumberFormat('zh-CN').format(value || 0)
}

export function formatUptime(seconds: number): string {
  const value = Math.max(0, Math.floor(seconds))
  const days = Math.floor(value / 86400)
  const hours = Math.floor((value % 86400) / 3600)
  const minutes = Math.floor((value % 3600) / 60)
  if (days > 0) return `${days} 天 ${hours} 小时`
  if (hours > 0) return `${hours} 小时 ${minutes} 分`
  if (minutes > 0) return `${minutes} 分 ${value % 60} 秒`
  return `${value} 秒`
}

export function formatTime(value?: string | number | null): string {
  if (value === null || value === undefined || value === '') return '—'
  const date = typeof value === 'number' ? new Date(value) : new Date(value)
  if (Number.isNaN(date.getTime())) return String(value)
  return date.toLocaleString('zh-CN', { hour12: false })
}

export function relativeTime(ms: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - ms) / 1000))
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  return `${Math.floor(hours / 24)} 天前`
}
