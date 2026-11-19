<template>
  <sl-search-form
    :value="keyword"
    class="mt-search-form"
    placeholder="名称、域名、类型或本地地址"
    aria-label="Proxy 查询"
    @update:value="emit('update:keyword', $event)"
    @submit="emit('submit')"
    @reset="emit('reset')"
  >
    <template #filters>
      <label class="mt-search-filter">
        <span class="mt-search-filter__label">类型</span>
        <sl-select
          :value="type"
          :options="proxyTypeOptionsAll"
          @update:value="emit('update:type', $event)"
        />
      </label>
    </template>

    <template #actions>
      <sl-button type="secondary" native-type="reset">重置</sl-button>
      <sl-button type="primary" native-type="submit">查询</sl-button>
    </template>
  </sl-search-form>
</template>

<script setup lang="ts">
import { SlSearchForm } from '@sailonix/ui/search-form'
import { SlSelect } from '@sailonix/ui/select'
import { SlButton } from '@sailonix/ui/button'
import { proxyTypeOptionsAll } from '../constants'

defineOptions({ name: 'ProxySearchBar' })

defineProps<{ keyword: string; type: string }>()

const emit = defineEmits<{
  'update:keyword': [value: string]
  'update:type': [value: string]
  submit: []
  reset: []
}>()
</script>
