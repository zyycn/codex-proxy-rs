<script setup lang="ts">
import type { ProxyTestExecutor } from '../composables/useProxyBatchTest'

import type { OutboundProxyRecord, OutboundProxyTest, RequestLocation } from '@/api'
import { ZButton, ZCheckbox, ZDialog, ZForm, ZFormItem, ZInput, ZSegmented, ZSwitch } from '@codex-proxy/ui'
import { Save, Square, Wifi } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import RequestLocationDetectButton from '@/components/RequestLocationDetectButton.vue'
import RequestLocationFields from '@/components/RequestLocationFields.vue'
import { useProxyBatchCreate } from '../composables/useProxyBatchCreate'
import ProxyBatchCreateFields from './ProxyBatchCreateFields.vue'
import ProxyBatchResults from './ProxyBatchResults.vue'

const props = defineProps<{
  proxy: OutboundProxyRecord | null
  saving: boolean
  testingConnection: boolean
  detectingLocation: boolean
  testResult: OutboundProxyTest | null
  testSavedProxies: ProxyTestExecutor
  reloadAfterCreate: () => Promise<unknown>
}>()

const emit = defineEmits<{
  save: []
  test: []
  detectLocation: []
  afterLeave: []
}>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const name = defineModel<string>('name', { required: true })
const proxyUrl = defineModel<string>('proxyUrl', { required: true })
const maxWebsocketConnections = defineModel<string>('maxWebsocketConnections', { required: true })
const customLocation = defineModel<boolean>('customLocation', { required: true })
const location = defineModel<RequestLocation>('location', { required: true })
const mode = shallowRef('single')
const testAfterCreate = shallowRef(true)
const modeOptions = computed(() => ([
  {
    label: t('proxies.editor.single'),
    value: 'single',
  },
  {
    label: t('proxies.editor.batch'),
    value: 'batch',
  },
]))
const batch = useProxyBatchCreate({
  test: (proxy, options, detectLocation) => props.testSavedProxies(proxy, options, detectLocation),
  reload: () => props.reloadAfterCreate(),
})
const {
  text: batchText,
  prefix: batchPrefix,
  protocol: batchProtocol,
  items: batchItems,
  busy: batchBusy,
  stopping: batchStopping,
  testingRequested,
  parsed,
  inputError,
  created,
  canEditFailures,
} = batch
const { items: batchTestItems, running: batchTesting } = batch.tests
const addableCount = computed(() => parsed.value.filter(row => !row.error).length)
const single = computed(() => Boolean(props.proxy) || mode.value === 'single')
const manualLocationWarning = computed(() => {
  const location = props.testResult?.location
  if (location?.status === 'conflict')
    return t('proxies.editor.timeZoneMismatch')
  return ''
})
const busy = computed(() => props.saving || props.testingConnection || props.detectingLocation || batchBusy.value)
const title = computed(() => props.proxy
  ? t('proxies.actions.edit')
  : t('proxies.editor.createTitle'))
const connectionDescription = computed(() => props.proxy
  ? t('proxies.editor.connectionHelp')
  : t('proxies.editor.protocolHelp'))

watch(open, (value) => {
  if (value) {
    mode.value = 'single'
    testAfterCreate.value = true
    batch.clear()
  }
})

function clearForm() {
  batch.clear()
  emit('afterLeave')
}
</script>

<template>
  <ZDialog v-model="open" :title="title" :description="proxy ? undefined : t('proxies.editor.createDescription')" width="42rem" :show-close="!busy" :close-on-click-modal="!busy" :close-on-press-escape="!busy" @closed="clearForm">
    <ZSegmented v-if="!proxy" v-model="mode" :options="modeOptions" :disabled="busy" :aria-label="t('proxies.editor.methodLabel')" class="mb-5 w-full" />
    <ZForm v-if="single" class="grid gap-5">
      <ZFormItem :label="t('proxies.editor.nameLabel')" required>
        <ZInput v-model="name" maxlength="100" :disabled="busy" :aria-label="t('proxies.editor.nameLabel')" :placeholder="t('proxies.editor.namePlaceholder')" />
      </ZFormItem>
      <ZFormItem :label="t('proxies.editor.proxyAddress')" :required="!proxy" :description="connectionDescription">
        <ZInput
          v-model="proxyUrl"
          show-password
          autocomplete="new-password"
          :disabled="busy"
          :aria-label="t('proxies.editor.proxyAddress')"
          :placeholder="t('proxies.editor.addressPlaceholder')"
        />
      </ZFormItem>
      <ZFormItem :label="t('proxies.editor.upstreamWsConnectionCap')" :description="t('proxies.editor.connectionLimitHelp')">
        <ZInput v-model="maxWebsocketConnections" :aria-label="t('proxies.editor.upstreamWsConnectionCap')" :placeholder="t('proxies.editor.noRestrictionsByDefault')" type="number" min="0" max="4294967295" step="1" :disabled="busy" />
      </ZFormItem>
      <div class="flex items-center justify-between gap-3">
        <ZSwitch v-model="customLocation" :label="t('proxies.editor.customTimeZoneLocation')" :disabled="busy" />
        <RequestLocationDetectButton :loading="detectingLocation" :disabled="busy" @detect="emit('detectLocation')" />
      </div>
      <p v-if="manualLocationWarning" class="m-0 text-cp-sm text-cp-warning-text" role="status">
        {{ manualLocationWarning }}
      </p>
      <RequestLocationFields v-if="customLocation" v-model="location" :disabled="busy" />
      <p v-if="proxy?.accountCount && proxyUrl.trim()" class="m-0 text-cp-sm text-cp-warning-text">
        {{ t('proxies.editor.affectedAccounts', { count: proxy.accountCount }) }}
      </p>
    </ZForm>
    <ProxyBatchCreateFields v-else-if="!batchItems.length" v-model="batchText" v-model:prefix="batchPrefix" v-model:protocol="batchProtocol" :rows="parsed" :error="inputError" :disabled="busy" />
    <div v-else class="grid min-w-0 gap-5">
      <ProxyBatchResults :items="batchItems" :tests="batchTestItems" />
      <p v-if="batchBusy" class="m-0 text-cp-xs text-cp-text-secondary">
        {{ t('proxies.editor.stopHelp') }}
      </p>
      <p v-if="batchItems.some(item => item.status === 'unknown')" class="m-0 text-cp-xs text-cp-warning-text" role="alert">
        {{ t('proxies.editor.pendingResultsHelp') }}
      </p>
    </div>
    <template #footer>
      <template v-if="single">
        <ZButton :disabled="busy" @click="open = false">
          {{ t('common.actions.cancel') }}
        </ZButton>
        <ZButton :loading="testingConnection" :disabled="saving || detectingLocation" @click="emit('test')">
          <template #icon>
            <Wifi class="size-4" />
          </template>
          {{ t('proxies.actions.test') }}
        </ZButton>
        <ZButton type="primary" :loading="saving" :disabled="testingConnection || detectingLocation" @click="emit('save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ t('proxies.editor.saveAction') }}
        </ZButton>
      </template>
      <template v-else-if="batchBusy">
        <ZButton :disabled="batchStopping" @click="batch.stop()">
          <Square class="size-3.5" />{{ batchStopping ? t('proxies.editor.waitForCurrentOperationToEnd') : t('proxies.editor.stopSubsequentOperations') }}
        </ZButton>
        <ZButton type="primary" loading>
          {{ batchTesting ? t('proxies.editor.testing') : t('proxies.editor.adding') }}
        </ZButton>
      </template>
      <template v-else-if="batchItems.length">
        <ZButton @click="open = false">
          {{ t('common.actions.close') }}
        </ZButton>
        <ZButton v-if="canEditFailures" @click="batch.editFailures()">
          {{ t('proxies.editor.editItemsNotAdded') }}
        </ZButton>
        <ZButton v-if="created.length && !testingRequested" type="primary" @click="batch.testCreated()">
          {{ t('proxies.editor.testAddedProxies', { count: created.length }) }}
        </ZButton>
      </template>
      <template v-else>
        <ZCheckbox v-model="testAfterCreate" class="mr-auto self-center" :label="t('proxies.editor.testAfterAdding')" />
        <ZButton @click="open = false">
          {{ t('common.actions.cancel') }}
        </ZButton>
        <ZButton type="primary" :disabled="!addableCount || Boolean(inputError)" @click="batch.submit(testAfterCreate)">
          {{ addableCount ? t('proxies.editor.add', { count: addableCount }) : t('proxies.editor.addProxy') }}
        </ZButton>
      </template>
    </template>
  </ZDialog>
</template>
