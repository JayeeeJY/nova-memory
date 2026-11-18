'use strict';
// Status is metadata only. Explicit verification never releases a backup's quarantine.
(()=>{
 let epoch=0,busy=false,quarantined=false;
 const status=$('recovery-status'),verify=$('recovery-verify');
 function clear(){epoch++;busy=false;quarantined=false;status.textContent='';verify.hidden=true;verify.disabled=false;}
 async function refresh(){
  if($('workspace').hidden)return;const e=epoch;
  try{const r=await api('/backup/status');if(e!==epoch||$('workspace').hidden)return;
   quarantined=!!r.recovery;verify.hidden=!quarantined;
   status.textContent=quarantined?'当前打开的是隔离备份。文件完整不代表已包含后来发生的删除和撤权；核对之前，AI 连接和自动整理保持关闭。':r.last_generation?'最近生成备份：'+new Date(r.last_generation.generated_at).toLocaleString('zh-CN')+'。仅记录生成，不代表文件已保存在其他设备或已完成恢复。':'尚无本机生成备份的记录。';
  }catch{if(e===epoch)status.textContent='备份状态暂时无法读取。';}
 }
 verify.addEventListener('click',async()=>{
  if(busy||!quarantined)return;const e=epoch;busy=true;verify.disabled=true;status.textContent='正在核对数据库和加密内容…';
  try{const r=await api('/backup/verify',{});if(e!==epoch||$('workspace').hidden)return;
   status.textContent=r.envelopes_authenticated&&r.sqlite_consistent&&r.control_state_matches&&r.may_automate===false?'当前副本完整性核对通过，仍保持隔离。后续删除与撤权尚未核对，未完成离机恢复验收。':'未能确认副本完整性，继续保持隔离。';
  }catch{if(e===epoch)status.textContent='副本未通过当前完整性核对，继续保持隔离。请保留原始备份用于排查。';}
  finally{if(e===epoch){busy=false;verify.disabled=false;}}
 });
 new MutationObserver(()=>{if($('workspace').hidden)clear();else refresh();}).observe($('workspace'),{attributes:true,attributeFilter:['hidden']});
 new MutationObserver(()=>{if($('backup-status').textContent)refresh();}).observe($('backup-status'),{childList:true});
 window.addEventListener('pagehide',clear);
})();
