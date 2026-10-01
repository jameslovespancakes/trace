// TypeScript worker: projects and the virtual file system (owner script).
//
// Everything the compiler reads comes from memory or from read-only mappings; nothing of the
// project runs (TypeScript 7 has no plugins) and nothing is written:
// * the snapshot sources (input.files) and the project's configuration texts (input.configs:
//   tsconfig*.json / jsconfig*.json / package.json, repository-relative) at their places under
//   the virtual workspace root; legacy compiler options TypeScript 7 removed are rewritten to
//   their nearest supported spelling in the in-memory copy (reported as diagnostics), and a
//   config without `types` gets the pre-TypeScript-6 default `["*"]` unless the project
//   installs a compiler 6 or newer (runtime types: installed `@types`, bundled Node types);
// * the compiler-bundled standard library `.d.ts`;
// * installed dependencies (input.modules: each `node_modules` of the repository mapped onto
//   the same place in the workspace) and trace's bundled Node types (input.bundled_types:
//   `@types/node` + `undici-types`, mapped only when the project has no own `@types/node`);
// * `realpath` follows symlinks inside a mapping (pnpm `.pnpm/<pkg>/node_modules`, workspace
//   links) and maps the real path back to its virtual place: inside a mapping, or into the
//   workspace when it points into the repository (input.repo_root).
// Projects: every tsconfig.json / jsconfig.json plus the configs their `references` name
// (solution-style roots). A source file belongs to the deepest project whose root files list
// it; files no project lists are analysed in a controlled project (allowJs, checkJs, ESNext,
// Bundler resolution, strict, maxNodeModuleJsDepth 1, types ["*"]).
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';

const CONTROLLED={allowJs:true,checkJs:true,noEmit:true,target:'ESNext',module:'ESNext',moduleResolution:'Bundler',jsx:'preserve',strict:true,maxNodeModuleJsDepth:1,types:['*']};

// JSON with comments and trailing commas (tsconfig.json) -> value; null when invalid.
export function parseJsonc(text){
  const s=String(text).replace(/^\uFEFF/,'');
  let out='',i=0,inString=false;
  while(i<s.length){
    const c=s[i];
    if(inString){
      out+=c;
      if(c==='\\'&&i+1<s.length){out+=s[i+1];i+=2;continue;}
      if(c==='"')inString=false;
      i++;continue;
    }
    if(c==='"'){inString=true;out+=c;i++;continue;}
    if(c==='/'&&s[i+1]==='/'){while(i<s.length&&s[i]!=='\n')i++;continue;}
    if(c==='/'&&s[i+1]==='*'){i+=2;while(i<s.length&&!(s[i]==='*'&&s[i+1]==='/')){if(s[i]==='\n')out+='\n';i++;}i+=2;continue;}
    if(c===','){
      let j=i+1;
      while(j<s.length){
        if(/\s/.test(s[j])){j++;continue;}
        if(s[j]==='/'&&s[j+1]==='/'){while(j<s.length&&s[j]!=='\n')j++;continue;}
        if(s[j]==='/'&&s[j+1]==='*'){j+=2;while(j<s.length&&!(s[j]==='*'&&s[j+1]==='/'))j++;j+=2;continue;}
        break;
      }
      if(s[j]==='}'||s[j]===']'){i++;continue;}
    }
    out+=c;i++;
  }
  try{return JSON.parse(out);}catch{return null;}
}

function baseName(p){const i=p.lastIndexOf('/');return i<0?p:p.slice(i+1);}
function dirName(p){const i=p.lastIndexOf('/');return i<0?'':p.slice(0,i);}
export function isProjectConfig(rel){const b=baseName(rel);return (b.startsWith('tsconfig')||b.startsWith('jsconfig'))&&b.endsWith('.json');}
function isRootConfig(rel){const b=baseName(rel);return b==='tsconfig.json'||b==='jsconfig.json';}
const lower=v=>typeof v==='string'?v.toLowerCase():v;
const relative=t=>t.startsWith('.')||t.startsWith('/')?t:'./'+t;

// Compiler options TypeScript 7 no longer accepts -> the nearest supported spelling.
export function sanitizeOptions(o,notes,where){
  if(['node','node10','classic'].includes(lower(o.moduleResolution))){
    notes.push(where+': moduleResolution '+o.moduleResolution+' read as bundler');
    o.moduleResolution='bundler';
    if(!['esnext','es2015','es6','es2020','es2022','preserve'].includes(lower(o.module)))o.module='preserve';
  }
  if(['amd','umd','system','none'].includes(lower(o.module))){notes.push(where+': module '+o.module+' read as preserve');o.module='preserve';}
  if(['es3','es5'].includes(lower(o.target))){notes.push(where+': target '+o.target+' read as es2015');o.target='es2015';}
  if(o.outFile!==undefined||o.out!==undefined){delete o.outFile;delete o.out;notes.push(where+': outFile ignored');}
  if(typeof o.baseUrl==='string'){
    const base=o.baseUrl.split('\\').join('/');
    if(o.paths&&typeof o.paths==='object'){
      for(const k of Object.keys(o.paths)){
        if(Array.isArray(o.paths[k]))o.paths[k]=o.paths[k].map(t=>relative(path.posix.join(base,String(t))));
      }
    } else o.paths={'*':[relative(path.posix.join(base,'*'))]};
    delete o.baseUrl;
    notes.push(where+': baseUrl folded into paths');
  }
  if(o.maxNodeModuleJsDepth===undefined)o.maxNodeModuleJsDepth=1;
}

export async function openProject(input, sdk){
const {API, TypeFlags, SymbolFlags} = await import(pathToFileURL(path.join(sdk,'dist/api/sync/api.js')));
const {SyntaxKind:K} = await import(pathToFileURL(path.join(sdk,'dist/ast/index.js')));
const {createVirtualFileSystem} = await import(pathToFileURL(path.join(sdk,'dist/api/fs.js')));
const {default:getExePath} = await import(pathToFileURL(path.join(sdk,'lib/getExePath.js')));
const norm = p=>path.resolve(p).replaceAll('\\','/');
const root=norm(input.workspace), files={}, original=new Map(), notes=[];
const rel=n=>n.startsWith(root+'/')?n.slice(root.length+1):n;
for (const f of input.files) {const name=norm(path.join(root,f.path)); files[name]=f.source; original.set(name,f.path);}
// The project's configuration (read, never executed).
const configTexts=new Map();
for (const c of input.configs??[]) {
  const name=norm(path.join(root,c.path));
  let text=c.text;
  if(isProjectConfig(c.path)){
    const parsed=parseJsonc(text);
    if(parsed&&typeof parsed==='object'){
      if(parsed.compilerOptions&&typeof parsed.compilerOptions==='object')sanitizeOptions(parsed.compilerOptions,notes,c.path);
      configTexts.set(name,parsed);
      text=JSON.stringify(parsed);
    }
  }
  files[name]=text;
}
// Projects to open: tsconfig.json / jsconfig.json and every config their references name.
const toOpen=[];
{
  const queue=[...configTexts.keys()].filter(n=>isRootConfig(rel(n)));
  while(queue.length){
    const n=queue.shift();
    if(toOpen.includes(n)||!configTexts.has(n))continue;
    toOpen.push(n);
    for(const r of configTexts.get(n).references??[]){
      if(!r||typeof r.path!=='string')continue;
      let target=norm(path.join(path.dirname(n),r.path));
      if(!configTexts.has(target))target=target+'/tsconfig.json';
      if(configTexts.has(target))queue.push(target);
    }
  }
  const depth=n=>n.split('/').length;
  toOpen.sort((a,b)=>depth(b)-depth(a)||(a<b?-1:a>b?1:0));
}
const controlled=root+'/.trace-worker/tsconfig.json';
const libraryDirectory=path.dirname(getExePath());
for(const name of fs.readdirSync(libraryDirectory)){
  if(name.endsWith('.d.ts'))files[norm(path.join(libraryDirectory,name))]=fs.readFileSync(path.join(libraryDirectory,name),'utf8');
}
let virtual=createVirtualFileSystem(files);
// Read-only mappings, most specific virtual directory first.
const win=process.platform==='win32';
const same=(a,b)=>win?a.toLowerCase()===b.toLowerCase():a===b;
const under=(p,dir)=>win?p.toLowerCase().startsWith(dir.toLowerCase()+path.sep):p.startsWith(dir+path.sep);
const safeReal=p=>{try{return fs.realpathSync(p);}catch{return p;}};
const modules=[...(input.modules??[]),...(input.bundled_types??[])]
  .map(m=>({v:norm(m.virtual),r:path.resolve(m.real)}))
  .map(m=>({...m,rr:safeReal(m.r)}))
  .sort((a,b)=>b.v.length-a.v.length);
const repoRoot=input.repo_root?safeReal(path.resolve(input.repo_root)):null;
function mapped(n){
  for(const m of modules){
    if(n===m.v)return m.r;
    if(n.startsWith(m.v+'/')){
      const real=path.resolve(m.r,n.slice(m.v.length+1));
      return real===m.r||real.startsWith(m.r+path.sep)?real:null;
    }
  }
  return null;
}
// A real path back to its virtual place: inside a mapping, else inside the repository.
function virtualOf(real){
  const byReal=[...modules].sort((a,b)=>b.rr.length-a.rr.length);
  for(const m of byReal){
    for(const base of [m.rr,m.r]){
      if(same(real,base))return m.v;
      if(under(real,base))return m.v+'/'+real.slice(base.length+1).split(path.sep).join('/');
    }
  }
  if(repoRoot){
    if(same(real,repoRoot))return root;
    if(under(real,repoRoot))return root+'/'+real.slice(repoRoot.length+1).split(path.sep).join('/');
  }
  return null;
}
const statOf=r=>{try{return fs.statSync(r);}catch{return null;}};
const depText=new Map();
function readDependency(r){
  if(depText.has(r))return depText.get(r);
  let text=null;
  try{if(statOf(r)?.isFile())text=fs.readFileSync(r,'utf8');}catch{text=null;}
  depText.set(r,text);return text;
}
function dependencyEntries(r){
  const out={files:[],directories:[]};
  let list=[];
  try{list=fs.readdirSync(r,{withFileTypes:true});}catch{return out;}
  for(const e of list){
    let dir=e.isDirectory(),file=e.isFile();
    if(e.isSymbolicLink()){const s=statOf(path.join(r,e.name));dir=!!s?.isDirectory();file=!!s?.isFile();}
    if(dir)out.directories.push(e.name);else if(file)out.files.push(e.name);
  }
  return out;
}
const vfs={
  readFile:p=>{const n=norm(p);if(Object.hasOwn(files,n))return files[n];const r=mapped(n);return r?readDependency(r):null;},
  fileExists:p=>{const n=norm(p);if(Object.hasOwn(files,n))return true;const r=mapped(n);return !!r&&!!statOf(r)?.isFile();},
  directoryExists:p=>{
    const n=norm(p);
    if(virtual.directoryExists?.(n))return true;
    if(modules.some(m=>m.v===n||m.v.startsWith(n+'/')))return true;
    const r=mapped(n);return !!r&&!!statOf(r)?.isDirectory();
  },
  getAccessibleEntries:p=>{
    const n=norm(p);const r=mapped(n);
    const base=r?dependencyEntries(r):(virtual.getAccessibleEntries?.(n)??{files:[],directories:[]});
    const directories=[...base.directories];
    for(const m of modules){
      if(m.v.startsWith(n+'/')){const child=m.v.slice(n.length+1).split('/')[0];if(!directories.includes(child))directories.push(child);}
    }
    return {files:base.files,directories};
  },
  realpath:p=>{
    const n=norm(p);const r=mapped(n);if(!r)return n;
    const real=safeReal(r);
    return virtualOf(real)??n;
  }};
// Runtime types of project configs (in-memory copies only): TypeScript 7 includes no `@types`
// package that `types` does not name, earlier compilers included every visible one. A config
// that sets no `types` (itself or through `extends`) and whose project installs a compiler
// older than 6, or none (JavaScript projects: editors read them with the older default), gets
// `types: ["*"]`, so the installed `@types` packages - and trace's bundled Node types when the
// project has none - describe the runtime as the project's own compiler sees it.
function resolveExtends(from,spec){
  if(typeof spec!=='string'||!spec)return null;
  const dir=path.posix.dirname(from);
  const exists=n=>configTexts.has(n)||vfs.fileExists(n);
  if(spec.startsWith('.')||spec.startsWith('/')){
    const t=norm(path.join(dir,spec));
    for(const c of [t,t+'.json',t+'/tsconfig.json'])if(exists(c))return c;
    return null;
  }
  for(let d=dir;d===root||d.startsWith(root+'/');d=path.posix.dirname(d)){
    const base=d+'/node_modules/'+spec;
    for(const c of [base,base+'.json',base+'/tsconfig.json'])if(exists(c))return c;
    if(d===root)break;
  }
  return null;
}
function declaresTypes(name,seen){
  if(seen.has(name)||seen.size>32)return false;
  seen.add(name);
  let doc=configTexts.get(name);
  if(!doc){const t=vfs.readFile(name);doc=t==null?null:parseJsonc(t);}
  if(!doc||typeof doc!=='object')return false;
  const o=doc.compilerOptions;
  if(o&&typeof o==='object'&&Object.hasOwn(o,'types'))return true;
  const ext=typeof doc.extends==='string'?[doc.extends]:Array.isArray(doc.extends)?doc.extends:[];
  return ext.some(e=>{const t=resolveExtends(name,e);return !!t&&declaresTypes(t,seen);});
}
// Major version of the compiler the project installs next to (or above) a config, or null.
function projectCompilerMajor(name){
  for(let d=path.posix.dirname(name);d===root||d.startsWith(root+'/');d=path.posix.dirname(d)){
    const t=vfs.readFile(d+'/node_modules/typescript/package.json');
    if(t!=null){
      const v=parseJsonc(t)?.version;
      const major=typeof v==='string'?Number.parseInt(v,10):Number.NaN;
      return Number.isFinite(major)?major:null;
    }
    if(d===root)break;
  }
  return null;
}
{
  const widened=[];
  for(const [name,doc] of configTexts){
    if(declaresTypes(name,new Set()))continue;
    const major=projectCompilerMajor(name);
    if(major!==null&&major>=6)continue;
    const o=doc.compilerOptions&&typeof doc.compilerOptions==='object'?doc.compilerOptions:{};
    doc.compilerOptions={...o,types:['*']};
    files[name]=JSON.stringify(doc);
    widened.push(rel(name));
  }
  if(widened.length){
    widened.sort();
    notes.push(widened.join(', ')+': no types option; every installed @types package included (the default before TypeScript 6)');
    virtual=createVirtualFileSystem(files);
  }
}
const api=new API({cwd:root,fs:vfs,collectTiming:true});
// Edits since the last snapshot (serve mode).
let pending={changed:new Set(),created:new Set(),deleted:new Set()};
function applyChanges(changed,deleted){
  for(const c of changed??[]){
    const name=norm(path.join(root,c.path));
    const existed=Object.hasOwn(files,name);
    files[name]=c.text;
    original.set(name,c.path);
    (existed?pending.changed:pending.created).add(name);
  }
  for(const d of deleted??[]){
    const name=norm(path.join(root,d));
    delete files[name];original.delete(name);pending.deleted.add(name);
  }
  virtual=createVirtualFileSystem(files);
}
function takeChanges(){
  const c={changed:[...pending.changed],created:[...pending.created],deleted:[...pending.deleted]};
  pending={changed:new Set(),created:new Set(),deleted:new Set()};
  return c.changed.length||c.created.length||c.deleted.length?c:null;
}
let controlledOpen=false;
// Open the projects for the current sources: {snapshot, groups: [{project, config, sources}], diagnostics}.
function load(){
  const diagnostics=[];
  const changes=takeChanges();
  // The API client caches parsed files per snapshot and carries them over by the server's
  // change report; after edits (measured with TypeScript 7.0.2) it kept the old tree of an
  // edited file while the server checked the new text. Drop the client cache after edits so
  // every tree comes from the server's current program (the server stays incremental).
  if(changes)api.clearSourceFileCache();
  const params=(open,extra)=>({openProjects:open,...(changes?{fileChanges:changes}:{}),...(extra??{})});
  let open=[...toOpen];
  let snapshot;
  try{snapshot=api.updateSnapshot(params(open));}
  catch(e){
    // Find the configs that do not load; their files go to the controlled project.
    open=[];
    for(const c of toOpen){
      try{const s=api.updateSnapshot({openProjects:[...open,c]});s.dispose();open.push(c);}
      catch(err){diagnostics.push({kind:'project_config_failed',message:rel(c)+': '+err});}
    }
    snapshot=api.updateSnapshot(params(open));
  }
  const owner=new Map();
  const projects=[];
  for(const c of open){
    const p=snapshot.getProject(c);
    if(!p){diagnostics.push({kind:'project_config_failed',message:rel(c)+': project did not load'});continue;}
    projects.push({config:c});
    for(const f of p.rootFiles??[]){const n=norm(f);if(original.has(n)&&!owner.has(n))owner.set(n,c);}
  }
  const leftovers=[...original.keys()].filter(n=>!owner.has(n));
  if(leftovers.length){
    const existed=Object.hasOwn(files,controlled);
    files[controlled]=JSON.stringify({compilerOptions:CONTROLLED,files:leftovers});
    virtual=createVirtualFileSystem(files);
    const next=api.updateSnapshot({openProjects:[...open,controlled],fileChanges:existed?{changed:[controlled]}:{created:[controlled]}});
    snapshot.dispose();snapshot=next;controlledOpen=true;
    for(const n of leftovers)owner.set(n,controlled);
    projects.push({config:controlled});
  } else if(controlledOpen){
    const next=api.updateSnapshot({openProjects:open,closeProjects:[controlled]});
    snapshot.dispose();snapshot=next;controlledOpen=false;
  }
  const groups=[];
  for(const {config} of projects){
    const project=snapshot.getProject(config);
    if(!project){
      if(config===controlled)throw Error('Controlled TypeScript project did not load');
      diagnostics.push({kind:'project_config_failed',message:rel(config)+': project did not load'});continue;
    }
    const sources=[...original.keys()].filter(n=>owner.get(n)===config).map(n=>project.program.getSourceFile(n)).filter(Boolean);
    groups.push({project,config,sources});
  }
  return {snapshot,groups,diagnostics};
}
// The real file behind a virtual path of a read-only mapping (installed node_modules, bundled
// types), or null: library files are reported by their real path, never the virtual one.
const realOf=p=>mapped(norm(p));
return {input,API,TypeFlags,SymbolFlags,K,norm,root,files,original,api,notes,load,applyChanges,realOf};
}
