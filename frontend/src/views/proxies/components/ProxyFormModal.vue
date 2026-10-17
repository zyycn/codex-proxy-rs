<script setup lang="ts">
import type { ProxyTestExecutor } from '../composables/useProxyBatchTest'
import type { OutboundProxyRecord, OutboundProxyTest, RequestLocation } from '@/api'
import { ZButton, ZCheckbox, ZDialog, ZForm, ZFormItem, ZInput, ZSegmented, ZSwitch } from '@codex-proxy/ui'
import { Save, Square, Wifi } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
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
const open = defineModel<boolean>({ required: true })
const name = defineModel<string>('name', { required: true })
const proxyUrl = defineModel<string>('proxyUrl', { required: true })
const maxWebsocketConnections = defineModel<string>('maxWebsocketConnections', { required: true })
const customLocation = defineModel<boolean>('customLocation', { required: true })
const location = defineModel<RequestLocation>('location', { required: true })
const mode = shallowRef('single')
const testAfterCreate = shallowRef(true)
const modeOptions = [{ label: '单条', value: 'single' }, { label: '批量', value: 'batch' }]
const batch = useProxyBatchCreate({ test: (proxy, options, detectLocation) => props.testSavedProxies(proxy, options, detectLocation), reload: () => props.reloadAfterCreate() })
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
    return 'IPv4 与 IPv6 出口时区不一致，请手动填写'
  if (location?.status === 'failed')
    return location.message
  return ''
})
const busy = computed(() => props.saving || props.testingConnection || props.detectingLocation || batchBusy.value)
const title = computed(() => props.proxy ? '编辑代理' : '新增代理')
const connectionDescription = computed(() => props.proxy
  ? '留空保留当前连接和认证信息，填写新地址时，请包含所需的用户名和密码'
  : '支持 HTTP、HTTPS、SOCKS5 和 SOCKS5H，可在地址中包含用户名和密码')

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
  <ZDialog v-model="open" :title="title" :description="proxy ? undefined : '添加代理，为账号配置网络出口'" width="42rem" :show-close="!busy" :close-on-click-modal="!busy" :close-on-press-escape="!busy" @closed="clearForm">
    <ZSegmented v-if="!proxy" v-model="mode" :options="modeOptions" :disabled="busy" aria-label="代理添加方式" class="mb-5 w-full" />
    <ZForm v-if="single" class="grid gap-5">
      <ZFormItem label="代理名称" required>
        <ZInput v-model="name" maxlength="100" :disabled="busy" aria-label="代理名称" placeholder="请输入代理名称" />
      </ZFormItem>
      <ZFormItem label="代理地址" :required="!proxy" :description="connectionDescription">
        <ZInput
          v-model="proxyUrl"
          show-password
          autocomplete="new-password"
          :disabled="busy"
          aria-label="代理地址"
          placeholder="请输入代理地址"
        />
      </ZFormItem>
      <ZFormItem label="上游 WS 连接上限" description="使用此代理的账号共享连接额度">
        <ZInput v-model="maxWebsocketConnections" aria-label="上游 WS 连接上限" placeholder="默认不限制" type="number" min="0" max="4294967295" step="1" :disabled="busy" />
      </ZFormItem>
      <div class="flex items-center justify-between gap-3">
        <ZSwitch v-model="customLocation" label="自定义时区位置" :disabled="busy" />
        <RequestLocationDetectButton :loading="detectingLocation" :disabled="busy" @detect="emit('detectLocation')" />
      </div>
      <p v-if="testResult?.success === false" class="m-0 text-cp-sm text-cp-error-text" role="alert">
        连接测试失败：{{ testResult.message }}
      </p>
      <p v-else-if="manualLocationWarning" class="m-0 text-cp-sm text-cp-warning-text" role="status">
        {{ manualLocationWarning }}
      </p>
      <RequestLocationFields v-if="customLocation" v-model="location" :disabled="busy" />
      <p v-if="proxy?.accountCount && proxyUrl.trim()" class="m-0 text-cp-sm text-cp-warning-text">
        将更新 {{ proxy.accountCount }} 个关联账号的出口
      </p>
    </ZForm>
    <ProxyBatchCreateFields v-else-if="!batchItems.length" v-model="batchText" v-model:prefix="batchPrefix" v-model:protocol="batchProtocol" :rows="parsed" :error="inputError" :disabled="busy" />
    <div v-else class="grid min-w-0 gap-5">
      <ProxyBatchResults :items="batchItems" :tests="batchTestItems" />
      <p v-if="batchItems.some(item => item.status === 'unknown')" class="m-0 text-cp-xs text-cp-warning-text" role="alert">
        结果待确认的条目可能已保存，请刷新列表核对后再添加
      </p>
    </div>
    <template #footer>
      <template v-if="single">
        <ZButton :disabled="busy" @click="open = false">
          取消
        </ZButton>
        <ZButton :loading="testingConnection" :disabled="saving || detectingLocation" @click="emit('test')">
          <template #icon>
            <Wifi class="size-4" />
          </template>
          测试连接
        </ZButton>
        <ZButton type="primary" :loading="saving" :disabled="testingConnection || detectingLocation" @click="emit('save')">
          <template #icon>
            <Save class="size-4" />
          </template>
          保存代理
        </ZButton>
      </template>
      <template v-else-if="batchBusy">
        <ZButton :disabled="batchStopping" @click="batch.stop()">
          <Square class="size-3.5" />{{ batchStopping ? '等待当前操作结束' : '停止后续操作' }}
        </ZButton>
        <ZButton type="primary" loading>
          {{ batchTesting ? '正在测试' : '正在添加' }}
        </ZButton>
      </template>
      <template v-else-if="batchItems.length">
        <ZButton @click="open = false">
          关闭
        </ZButton>
        <ZButton v-if="canEditFailures" @click="batch.editFailures()">
          修改未添加项
        </ZButton>
        <ZButton v-if="created.length && !testingRequested" type="primary" @click="batch.testCreated()">
          测试已添加的 {{ created.length }} 条
        </ZButton>
      </template>
      <template v-else>
        <ZCheckbox v-model="testAfterCreate" class="mr-auto self-center" label="添加后测试" />
        <ZButton @click="open = false">
          取消
        </ZButton>
        <ZButton type="primary" :disabled="!addableCount || Boolean(inputError)" @click="batch.submit(testAfterCreate)">
          {{ addableCount ? `添加 ${addableCount} 条` : '添加代理' }}
        </ZButton>
      </template>
    </template>
  </ZDialog>
</template>
