<script setup lang="ts">
import type { InstalledPlugin } from '../utils/catalog'
import type { PluginArtifact, PluginInstance, PluginManagementView } from '@/api'
import { ArrowInDownSquareHalf } from '@boxicons/vue'
import { BaseButton, BaseEmpty, BaseIconButton, BaseModal, BaseSegmented, BaseTag } from '@codex-proxy/ui'
import { ArrowUpRight, CircleAlert, History, Layers, Play, Power, RefreshCw, Settings2 } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { RouterLink } from 'vue-router'
import { configurationStatus, currentPluginInstance, hasPluginSettings, PLUGIN_STATUS_LABELS, pluginStatusType } from '../utils/catalog'
import { artifactForInstance } from '../utils/model'
import { pluginPageLocation } from '../utils/navigation'
import PluginConfigurationSummary from './PluginConfigurationSummary.vue'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginLegacyConfigurations from './PluginLegacyConfigurations.vue'
import PluginVersionsPanel from './PluginVersionsPanel.vue'

const props = defineProps<{ plugin: InstalledPlugin | null, initialSection: 'configurations' | 'versions', views: PluginManagementView[], busy: boolean }>()
defineEmits<{
  accept: [artifact: PluginArtifact]
  edit: [instance: PluginInstance]
  enable: [instance: PluginInstance]
  disable: [instance: PluginInstance]
  deleteConfiguration: [instance: PluginInstance]
  deleteVersion: [artifact: PluginArtifact]
  rollback: [instance: PluginInstance]
  installVersion: [plugin: InstalledPlugin]
  checkUpdate: [plugin: InstalledPlugin]
  switchVersion: [artifact: PluginArtifact]
  uninstall: [plugin: InstalledPlugin]
}>()
const open = defineModel<boolean>({ required: true })
const section = shallowRef('configurations')
const sections = [{ label: '概览', value: 'configurations', icon: Settings2 }, { label: '版本', value: 'versions', icon: Layers }]
const current = computed(() => props.plugin && currentPluginInstance(props.plugin))
const legacy = computed(() => props.plugin?.configurations.filter(instance => instance.id !== current.value?.id) ?? [])
const currentArtifact = computed(() => current.value && artifactForInstance(current.value, props.plugin?.artifacts ?? []))
const acceptedArtifact = computed(() => props.plugin?.artifacts.find(artifact => artifact.acceptedAt) ?? null)
const pendingArtifact = computed(() => props.plugin?.artifacts.find(artifact => !artifact.acceptedAt) ?? null)
const viewByInstance = computed(() => new Map(props.views.map(view => [view.target.instanceId, view])))
function capabilities(instance: PluginInstance) {
  return artifactForInstance(instance, props.plugin?.artifacts ?? [])?.metadata.contributes ?? {}
}
function configurationNotes(instance: PluginInstance) {
  const notes: string[] = []
  if (instance.runtime.failure)
    notes.push(instance.runtime.failure.message)
  if (configurationStatus(instance) === 'pending')
    notes.push('配置已保存，等待生效，状态会自动刷新')
  if (configurationStatus(instance) === 'unconfigured')
    notes.push('补充必填配置后即可启用')
  if (instance.runtime.drainingRevisions.length)
    notes.push('旧版本仍有进行中的调用，完成前暂不能删除')
  return notes
}
watch(open, (value) => {
  if (value)
    section.value = props.initialSection
})
watch(() => props.initialSection, value => section.value = value)
</script>

<template>
  <BaseModal v-model="open" :title="plugin?.artifact.metadata.displayName ?? '插件详情'" :description="plugin?.artifact.metadata.description" size="md-wide" :dismissible="!busy">
    <div v-if="plugin" class="grid gap-5">
      <div class="flex flex-wrap items-center gap-3">
        <BaseSegmented v-model="section" :options="sections" label="插件详情分区" class="w-44" />
        <div v-if="plugin.artifact.source.kind !== 'builtin'" class="ml-auto flex items-center gap-2">
          <BaseIconButton v-if="plugin.source?.source.kind === 'github' || plugin.source?.source.kind === 'url'" label="检查更新" variant="secondary" :disabled="busy" @click="$emit('checkUpdate', plugin)">
            <RefreshCw class="size-4" />
          </BaseIconButton>
          <BaseIconButton label="手动安装版本" variant="secondary" :disabled="busy" @click="$emit('installVersion', plugin)">
            <ArrowInDownSquareHalf pack="filled" class="size-5" />
          </BaseIconButton>
        </div>
      </div>
      <template v-if="section === 'configurations'">
        <BaseEmpty v-if="!plugin.configurations.length" :title="acceptedArtifact ? '插件尚未初始化' : '插件待安装'" size="sm" surface="inset">
          <template #action>
            <BaseButton v-if="acceptedArtifact" variant="primary" @click="$emit('accept', acceptedArtifact)">
              初始化插件
            </BaseButton>
            <BaseButton v-else-if="pendingArtifact" variant="primary" @click="$emit('accept', pendingArtifact)">
              安装
            </BaseButton>
          </template>
        </BaseEmpty>
        <article v-for="instance in current ? [current] : []" :key="instance.id" class="grid min-h-36 content-between gap-3 rounded-cp bg-cp-fill-alter p-4">
          <div class="flex flex-wrap items-center gap-2">
            <strong class="min-w-0 flex-1 wrap-break-word text-cp-sm">当前版本</strong>
            <BaseTag>{{ artifactForInstance(instance, plugin.artifacts)?.metadata.version ?? '版本不可用' }}</BaseTag>
            <BaseTag :type="pluginStatusType(configurationStatus(instance))">
              <CircleAlert v-if="configurationStatus(instance) === 'failed'" class="mr-1 size-3.5" />
              {{ PLUGIN_STATUS_LABELS[configurationStatus(instance)] }}
            </BaseTag>
            <PluginHelpPopover v-if="configurationNotes(instance).length" :label="`${instance.name}配置状态说明`">
              <p v-for="note in configurationNotes(instance)" :key="note" class="m-0">
                {{ note }}
              </p>
            </PluginHelpPopover>
          </div>
          <PluginConfigurationSummary
            :instance="instance"
            :metadata="artifactForInstance(instance, plugin.artifacts)?.metadata"
            :show-command="configurationStatus(instance) === 'enabled' && Boolean(capabilities(instance).command_line)"
          />
          <div class="flex flex-wrap items-center gap-2">
            <div v-if="configurationStatus(instance) === 'enabled'" class="mr-auto flex min-w-0 flex-wrap items-center gap-2">
              <RouterLink v-for="page in viewByInstance.get(instance.id)?.pages ?? []" :key="page.id" :to="pluginPageLocation(viewByInstance.get(instance.id)!, page)" class="inline-flex h-cp-control-sm min-w-0 items-center gap-1.5 rounded-cp bg-cp-primary-container px-2.5 text-cp-sm leading-none text-cp-primary-on-container no-underline outline-none transition-colors hover:bg-cp-primary-container-hover focus-visible:ring-2 focus-visible:ring-cp-control-outline motion-reduce:transition-none" @click="open = false">
                <span class="truncate">{{ page.title }}</span>
                <ArrowUpRight class="size-3.5 shrink-0" />
              </RouterLink>
            </div>
            <BaseIconButton v-if="instance.configurationRequired || (currentArtifact && hasPluginSettings(currentArtifact))" size="sm" variant="secondary" label="设置" :disabled="busy" @click="$emit('edit', instance)">
              <Settings2 class="size-4" />
            </BaseIconButton>
            <BaseIconButton v-if="configurationStatus(instance) === 'failed'" label="重新启动" size="sm" variant="secondary" :disabled="busy" @click="$emit('enable', instance)">
              <RefreshCw class="size-4" />
            </BaseIconButton>
            <BaseIconButton v-if="instance.enabled" label="停用" size="sm" variant="secondary" :disabled="busy" @click="$emit('disable', instance)">
              <Power class="size-4" />
            </BaseIconButton>
            <BaseIconButton v-else :label="instance.configurationRequired ? '完成设置并启用' : '启用'" size="sm" variant="secondary" :disabled="busy" @click="$emit('enable', instance)">
              <Play class="size-4" />
            </BaseIconButton>
            <BaseIconButton v-if="plugin.artifacts.length > 1" label="回退版本" size="sm" variant="secondary" :disabled="busy" @click="$emit('rollback', instance)">
              <History class="size-4" />
            </BaseIconButton>
          </div>
        </article>
        <PluginLegacyConfigurations
          :instances="legacy"
          :artifacts="plugin.artifacts"
          :busy="busy"
          @edit="$emit('edit', $event)"
          @enable="$emit('enable', $event)"
          @disable="$emit('disable', $event)"
          @delete="$emit('deleteConfiguration', $event)"
        />
      </template>
      <PluginVersionsPanel v-else :plugin="plugin" :busy="busy" @accept="$emit('accept', $event)" @switch-version="$emit('switchVersion', $event)" @delete-version="$emit('deleteVersion', $event)" />
    </div>
    <template #footer>
      <BaseButton v-if="plugin && plugin.artifacts.every(artifact => artifact.source.kind !== 'builtin')" variant="destructive" :disabled="busy" @click="$emit('uninstall', plugin)">
        卸载插件
      </BaseButton>
      <BaseButton variant="secondary" :disabled="busy" @click="open = false">
        关闭
      </BaseButton>
    </template>
  </BaseModal>
</template>
