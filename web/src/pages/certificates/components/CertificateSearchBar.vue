<template>
  <sl-search-form
    :value="keyword"
    class="mt-search-form"
    placeholder="域名、proxy 或签发机构"
    aria-label="证书查询"
    @update:value="emit('update:keyword', $event)"
    @submit="emit('submit')"
    @reset="emit('reset')"
  >
    <template #filters>
      <label class="mt-search-filter">
        <span class="mt-search-filter__label">状态</span>
        <sl-select
          :value="status"
          :options="statusOptions"
          @update:value="emit('update:status', $event)"
        />
      </label>
    </template>
  </sl-search-form>
</template>

<script setup lang="ts">
import { SlSearchForm } from '@sailonix/ui/search-form'
import { SlSelect } from '@sailonix/ui/select'

defineOptions({ name: 'CertificateSearchBar' })

defineProps<{ keyword: string; status: string }>()

const emit = defineEmits<{
  'update:keyword': [value: string]
  'update:status': [value: string]
  submit: []
  reset: []
}>()

const statusOptions = [
  { value: 'all', label: '全部状态' },
  { value: 'managed', label: '托管中' },
  { value: 'expiring', label: '即将到期' },
  { value: 'pending', label: '等待签发' },
  { value: 'expired', label: '已过期' },
]
</script>
