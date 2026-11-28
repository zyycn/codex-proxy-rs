<script setup lang="ts">
import type { TargetTestProxy } from '../composables/useProxyTargetTest'
import type { ProxyTestTarget, ProxyTestTargets } from '@/api'
import { ZButton, ZCheckbox, ZDialog, ZSelect } from '@codex-proxy/ui'
import { Settings2 } from '@lucide/vue'
import { computed, ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { getProxyTestTargets, saveProxyTestTargets } from '@/api'
import { ApiError } from '@/api/request'
import { useRequestState } from '@/composables/useRequestState'
import { useProxyTargetTest } from '../composables/useProxyTargetTest'
import ProxyTargetEditor from './ProxyTargetEditor.vue'
import ProxyTargetResultCard from './ProxyTargetResultCard.vue'

const props = defineProps<{ proxy: TargetTestProxy, selectedProxies: TargetTestProxy[] }>()
const emit = defineEmits<{ closed: [] }>()
const { t } = useI18n({ useScope: 'global' })
const open = shallowRef(true)
const includeSelection = shallowRef(false)
const settings = shallowRef<ProxyTestTargets>()
const selected = shallowRef<string>()
const draft = ref<ProxyTestTarget[]>()
const stale = shallowRef(false)
const catalog = useRequestState()
const { loading, error } = catalog
const testProxies = computed(() => includeSelection.value
  ? props.selectedProxies
  : [props.proxy])
const { byProxyId, clearResults, running, start, cancel } = useProxyTargetTest(() => {
  stale.value = true
})
const options = computed(() => settings.value?.targets.map(({ name, url }) => ({ label: name, value: name, description: url })) ?? [])
function accept(data: ProxyTestTargets) {
  settings.value = data
  if (!data.targets.some(target => target.name === selected.value))
    selected.value = data.targets[0]?.name
  clearResults()
  stale.value = false
}

async function load() {
  const id = catalog.start()
  try {
    const data = await getProxyTestTargets({ silent: true, signal: catalog.signal })
    if (catalog.isCurrent(id)) {
      accept(data)
      if (draft.value)
        edit()
    }
  }
  catch (cause) {
    catalog.fail(id, cause)
  }
  finally {
    catalog.finish(id)
  }
}

function edit() {
  const targets = settings.value?.targets ?? []
  draft.value = targets.length
    ? targets.map(target => ({ ...target }))
    : [
        { name: '', url: '' },
      ]
}

async function save() {
  if (!draft.value || !settings.value || stale.value || loading.value)
    return
  const id = catalog.start()
  try {
    const data = await saveProxyTestTargets({ revision: settings.value.revision, targets: draft.value }, { signal: catalog.signal, silent: true })
    if (catalog.isCurrent(id)) {
      accept(data)
      draft.value = undefined
    }
  }
  catch (cause) {
    catalog.fail(id, cause)
    if (cause instanceof ApiError && cause.status === 409)
      stale.value = true
  }
  finally {
    catalog.finish(id)
  }
}

function cancelRequests() {
  catalog.invalidate({ resetLoading: false })
  cancel({ resetLoading: false })
}

function test() {
  if (!settings.value || !selected.value || running.value || stale.value)
    return
  void start(testProxies.value, selected.value, settings.value.revision)
}
</script>

<template>
  <ZDialog v-model="open" :title="t(draft ? 'proxies.targets.manage' : 'proxies.targets.custom')" :description="draft ? t('proxies.targets.shared') : undefined" width="38rem" :show-close="!draft || !loading" :close-on-click-modal="!draft || !loading" :close-on-press-escape="!draft || !loading" @open="load" @close="cancelRequests" @closed="emit('closed')">
    <ProxyTargetEditor v-if="draft" v-model="draft" :disabled="loading || stale" />
    <template v-else-if="settings">
      <div v-if="selected" class="grid gap-2">
        <span class="text-z-sm font-emphasis text-z-text">{{ t('proxies.targets.site') }}</span>
        <ZSelect v-model="selected" class="w-full" :aria-label="t('proxies.targets.site')" :options="options" :disabled="running || loading || stale" @change="clearResults" />
      </div>
      <p v-else class="m-0 text-z-sm text-z-text-secondary">
        {{ t('proxies.targets.empty') }}
      </p>
      <ZCheckbox v-if="props.selectedProxies.length && selected" v-model="includeSelection" class="mt-3" :disabled="running || loading" :label="t('proxies.targets.includeSelected', { count: props.selectedProxies.length })" @update:model-value="clearResults" />
    </template>
    <p v-else-if="loading" class="m-0 text-z-sm text-z-text-secondary" role="status">
      {{ t('common.status.loading') }}
    </p>
    <p v-if="error" class="m-0 text-z-sm text-z-error" role="alert">
      {{ error }}
    </p>
    <ul v-if="!draft" class="mt-5 mb-0 grid list-none gap-2 p-0" aria-live="polite">
      <ProxyTargetResultCard v-for="item in testProxies" :key="item.id" :proxy="item" :row="byProxyId.get(item.id)" :batch-running="running" />
    </ul>
    <template #footer>
      <ZButton v-if="!draft && selected" class="mr-auto" :disabled="running || loading || stale" @click="edit">
        <template #icon>
          <Settings2 class="size-4" />
        </template>
        {{ t('proxies.targets.manage') }}
      </ZButton>
      <ZButton v-if="draft" :disabled="loading" @click="draft = undefined; error = ''">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton v-if="stale || (!settings && !loading)" :disabled="running" :loading="loading" @click="load">
        {{ t('common.actions.refresh') }}
      </ZButton>
      <ZButton v-else-if="draft" type="primary" :loading="loading" @click="save">
        {{ t('common.actions.save') }}
      </ZButton>
      <ZButton v-else-if="settings && !selected" type="primary" @click="edit">
        {{ t('proxies.targets.add') }}
      </ZButton>
      <ZButton v-else-if="selected" type="primary" :loading="running" :disabled="loading" @click="test">
        {{ t('proxies.targets.run') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
