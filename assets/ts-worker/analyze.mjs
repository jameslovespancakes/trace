// TypeScript worker: analysis and references modes (owner script). Returns the output object
// (main.mjs prints it / answers it in serve mode). Library hooks: fntype.mjs (declared
// parameter types of external signatures) and external.mjs (library call locations).
//
// request = {names?: [declared names], query?: [repository-relative paths to visit],
//            references?: {file, start_byte}}
// Only the queried files are visited (every file of every project is still declared, so
// targets anywhere resolve); without `query` every file is visited.
// Checker rules (facts from the checker, no tables):
// * `require("x")` is the runtime's module import, not an unknown call (external);
// * `new C()` of a class without its own constructor -> `constructor` edge to the class;
// * a call whose signature comes from a type (interface / alias call signature) while the
//   callee variable or property holds one repository function - its initializer and every
//   write in the repository assign that function (`text: TextRespond = (...) => ...`,
//   `this[method] = (...) => ...` in a constructor; assigned.mjs) -> `calls` edge to it;
// * a call of a call whose callee returns one repository function (`compose(stack)(ctx)`;
//   assigned.mjs) -> `calls` edge to that function;
// * a property read resolving to a get accessor -> `property_get` edge to the getter;
// * a tagged template f`x` is a call of its tag `f` (the checker resolves its signature like
//   any call) and goes through every rule above.
// Checker calls are batched per file (types of all callees, symbols of all rule candidates).
import {callbackParams} from './fntype.mjs';
import {libraryCall,packageOf,readableLibraryFile} from './external.mjs';
import {assignedFunctions} from './assigned.mjs';
import fs from 'node:fs';

// Library files through a read-only mapping are reported by their real path (the installed
// package in node_modules, trace's bundled types): the virtual workspace path exists only in
// the worker's file system, so neither its package.json (version) nor its implementation
// (readable source for derivation) can be read there, and it would name trace's cache.
// Readable = the file or its installed implementation (sibling, package manifest entry,
// `@types` package) is JavaScript trace-library derives from (one rule: external.mjs).
function realLibraryFiles(ctx, files){
  for(const f of files){
    const real=ctx.realOf?.(f.path);
    if(!real||real===f.path)continue;
    f.path=real;
    const pkg=packageOf(real);
    if(pkg)f.version=pkg.version;
    f.readable=readableLibraryFile(real);
  }
}
export function analyze(ctx, request){
const {TypeFlags,SymbolFlags,K,norm,original}=ctx;
const callback_params=[],library_files=[],library_calls=[];
const names=new Set(request.names??[]);
const refQuery=request.references??null;
const queried=request.query?new Set(request.query):null;
const symbols={},edges=[],unresolved=[],uses=[],diagnostics=[];
for(const m of ctx.notes)diagnostics.push({kind:'project_config_adjusted',message:m});
const callable=new Set([K.FunctionDeclaration,K.FunctionExpression,K.ArrowFunction,K.MethodDeclaration,K.Constructor,K.GetAccessor,K.SetAccessor]);
const functionExpression=new Set([K.FunctionExpression,K.ArrowFunction,K.ClassExpression]);
// Declarations that are use targets without being owners (no body / not callable).
const typeTargets=new Set([K.InterfaceDeclaration,K.TypeAliasDeclaration,K.EnumDeclaration,K.MethodSignature,K.PropertySignature,K.PropertyDeclaration,K.MethodDeclaration,K.ModuleDeclaration,K.VariableDeclaration]);
// Nodes whose `name` child is the declared identifier (not a use).
const declarationKinds=new Set([K.FunctionDeclaration,K.FunctionExpression,K.MethodDeclaration,K.ClassDeclaration,K.ClassExpression,K.InterfaceDeclaration,K.TypeAliasDeclaration,K.EnumDeclaration,K.EnumMember,K.VariableDeclaration,K.PropertyDeclaration,K.PropertySignature,K.MethodSignature,K.Parameter,K.GetAccessor,K.SetAccessor,K.ModuleDeclaration,K.PropertyAssignment,K.ShorthandPropertyAssignment,K.BindingElement,K.TypeParameter,K.NamespaceImport,K.ImportEqualsDeclaration,K.NamespaceExport]);
const declarations=new Map();
// Names of get accessors declared in the repository (property reads worth a checker look).
const getterNames=new Set();
function key(n){return norm(n.getSourceFile().fileName)+':'+n.pos+':'+n.end;}
// UTF-16 position <-> UTF-8 byte offset, cached per source file (ASCII files are identity).
const offsets=new Map();
function table(sf){
  let t=offsets.get(sf.fileName);
  if(t!==undefined)return t;
  const text=sf.text;
  if(Buffer.byteLength(text,'utf8')===text.length){t=null;}
  else{
    t=new Uint32Array(text.length+1);let b=0;
    for(let i=0;i<text.length;i++){
      t[i]=b;const c=text.charCodeAt(i);
      if(c<0x80)b+=1;else if(c<0x800)b+=2;else if(c>=0xD800&&c<=0xDBFF){t[i+1]=b;b+=4;i++;}else if(c>=0xDC00&&c<=0xDFFF)b+=3;else b+=3;
    }
    t[text.length]=b;
  }
  offsets.set(sf.fileName,t);return t;
}
function bytes(sf,pos){const t=table(sf);return t?t[pos]:pos;}
function charPos(sf,byte){
  const t=table(sf);if(!t)return byte;
  let lo=0,hi=t.length-1;
  while(lo<hi){const mid=(lo+hi+1)>>1;if(t[mid]<=byte)lo=mid;else hi=mid-1;}
  return lo;
}
function lineOf(sf,pos){return sf.getLineAndCharacterOfPosition(pos).line+1;}
function fileOf(n){return original.get(norm(n.getSourceFile().fileName));}
function text(n,sf){return n.text??n.getText(sf);}
// Name of a function expression / arrow / class expression from its binding (trace-syntax rule).
function bindingName(n,sf){
  const p=n.parent;if(!p)return null;
  if((p.kind===K.VariableDeclaration||p.kind===K.PropertyDeclaration||p.kind===K.PropertyAssignment)&&p.initializer===n&&p.name)return p.name.getText(sf);
  if(p.kind===K.BinaryExpression&&p.right===n&&p.operatorToken?.kind===K.EqualsToken){
    const left=p.left;
    if(left.kind===K.PropertyAccessExpression)return left.name.getText(sf);
    if(left.kind===K.Identifier)return left.getText(sf);
  }
  return null;
}
function symbolEntry(n,name,scope,kind){
  const sf=n.getSourceFile(), file=fileOf(n); if(!file)return null;
  const start=bytes(sf,n.getStart(sf)), end=bytes(sf,n.end), id='ts:'+file+':'+start+':'+end;
  symbols[id]??={id,file,name,qualified_name:[...scope,name].join('.'),kind,language:file.endsWith('.tsx')?'tsx':file.endsWith('.ts')||file.endsWith('.mts')||file.endsWith('.cts')?'typescript':'javascript',start_byte:start,end_byte:end,source_line:lineOf(sf,n.getStart(sf)),end_line:lineOf(sf,Math.max(n.end-1,0)),backend_id:key(n),execution_model:n.asteriskToken?'generator':'ordinary'};
  return id;
}
function add(n,scope){
  const sf=n.getSourceFile();
  let name=functionExpression.has(n.kind)?(bindingName(n,sf)??n.name?.getText(sf)):n.name?.getText(sf);
  if(!name && n.kind===K.Constructor)name='constructor';
  name ||= '<anonymous@'+n.getStart(sf)+'>';
  const id=symbolEntry(n,name,scope,n.kind===K.ClassDeclaration||n.kind===K.ClassExpression?'class':'function');
  if(id)declarations.set(key(n),id);
  if(n.kind===K.GetAccessor&&n.name)getterNames.add(n.name.getText(sf));
  return id;
}
const moduleIds=new Map();
function moduleOwner(sf){
  const file=original.get(norm(sf.fileName));if(!file)return null;
  let id=moduleIds.get(file);
  if(!id){
    id='ts:'+file+':module';
    symbols[id]={id,file,name:'<module>',qualified_name:'<module>',kind:'module',start_byte:0,end_byte:Buffer.byteLength(sf.text,'utf8'),source_line:1,end_line:lineOf(sf,Math.max(sf.text.length-1,0)),backend_id:id,execution_model:'ordinary'};
    moduleIds.set(file,id);
  }
  return id;
}
// The callee of a call: `f` of `f(x)`, `new f(x)` and of the tagged template f`x` (a call of
// `f` with the template's strings and values).
function calleeOf(n){return n.kind===K.TaggedTemplateExpression?n.tag:n.expression;}
const callKinds=new Set([K.CallExpression,K.NewExpression,K.TaggedTemplateExpression]);
function isCallee(n){
  const p=n.parent;if(!p)return false;
  if(callKinds.has(p.kind)&&calleeOf(p)===n)return true;
  if(p.kind===K.PropertyAccessExpression&&p.name===n){const g=p.parent;return !!g&&callKinds.has(g.kind)&&calleeOf(g)===p;}
  return false;
}
function isAssignment(p){return p?.kind===K.BinaryExpression&&p.operatorToken&&p.operatorToken.kind>=K.FirstAssignment&&p.operatorToken.kind<=K.LastAssignment;}
function isUpdate(p){return (p?.kind===K.PrefixUnaryExpression||p?.kind===K.PostfixUnaryExpression)&&(p.operator===K.PlusPlusToken||p.operator===K.MinusMinusToken);}
// read | write | import | reexport, or null (declaration names, renamed import/export sides).
function useKind(n){
  const p=n.parent;if(!p)return 'read';
  if(p.kind===K.ImportSpecifier)return p.name===n?'import':null;
  if(p.kind===K.ImportClause)return p.name===n?'import':null;
  if(p.kind===K.ExportSpecifier)return (p.propertyName??p.name)===n?'reexport':null;
  if(declarationKinds.has(p.kind)&&p.name===n)return null;
  if(p.kind===K.PropertyAccessExpression&&p.name===n){
    const g=p.parent;
    if(isAssignment(g)&&g.left===p)return 'write';
    if(isUpdate(g))return 'write';
    return 'read';
  }
  if(isAssignment(p)&&p.left===n)return 'write';
  if(isUpdate(p))return 'write';
  return 'read';
}
function handleKey(h){return h.path+'#'+h.index;}
function identifiersNamed(sf,wanted){
  const out=[];
  (function walk(n){
    if((n.kind===K.Identifier||n.kind===K.PrivateIdentifier)&&wanted(text(n,sf)))out.push(n);
    n.forEachChild(walk);
  })(sf);
  return out;
}
// `require("x")`: the runtime's module import.
function isRequire(n){
  const e=n.expression;
  return n.kind===K.CallExpression&&e?.kind===K.Identifier&&e.text==='require'&&(n.arguments?.length===1)&&(n.arguments[0].kind===K.StringLiteral||n.arguments[0].kind===K.NoSubstitutionTemplateLiteral);
}
// The start of a call / member chain (`request(app).get('/').expect` -> `request`); a
// `require('x')` call is a start of its own.
function chainRoot(e){
  while(e){
    if(e.kind===K.CallExpression){if(isRequire(e))return e;e=e.expression;continue;}
    if(e.kind===K.PropertyAccessExpression||e.kind===K.ElementAccessExpression||e.kind===K.ParenthesizedExpression||e.kind===K.NonNullExpression||e.kind===K.AsExpression||e.kind===K.AwaitExpression){e=e.expression;continue;}
    return e;
  }
  return null;
}
// `require('pkg')` / `import ... from 'pkg'`: a package specifier (not a relative or absolute path).
function isPackageSpecifier(s){
  if(typeof s!=='string'||s.length===0||s.startsWith('.')||s.startsWith('/')||s.startsWith('\\'))return false;
  // `C:\x` / `C:/x`: an absolute Windows path, not a package.
  return !(s.length>2&&s[1]===':'&&(s[2]==='\\'||s[2]==='/'));
}
// The identifier naming a callee (`f`, `o.f`, `new C`).
function calleeName(n){
  const e=calleeOf(n);if(!e)return null;
  if(e.kind===K.Identifier)return e;
  if(e.kind===K.PropertyAccessExpression)return e.name;
  return null;
}

const {snapshot,groups,diagnostics:loadDiagnostics}=ctx.load();
diagnostics.push(...loadDiagnostics);
try{
if(refQuery){
  // References mode: per project that contains the queried file, every identifier with the
  // target's text whose symbol (aliases followed) shares a declaration with the target.
  let complete=true, found=false;
  const results=new Map();
  for(const {project,sources} of groups){
    const {program,checker}=project;
    const queryPath=[...original.entries()].find(([,p])=>p===refQuery.file)?.[0];
    const target=queryPath?program.getSourceFile(queryPath):null;
    if(!target)continue;
    const followAlias=aliasFollower(checker);
    const pos=charPos(target,refQuery.start_byte);
    let n=target, descended=true;
    while(descended&&n.kind!==K.Identifier&&n.kind!==K.PrivateIdentifier){
      descended=false;let next=null;
      n.forEachChild(c=>{if(!next&&c.getStart(target)<=pos&&pos<c.end)next=c;});
      if(next){n=next;descended=true;}
    }
    const ident=n.kind===K.Identifier||n.kind===K.PrivateIdentifier?n:null;
    const targetSymbol=ident?followAlias(checker.getSymbolAtLocation(ident)):null;
    if(!targetSymbol)continue;
    found=true;
    const name=text(ident,target);
    const declKeys=new Set((targetSymbol.declarations??[]).map(handleKey));
    const declNodes=new Set((targetSymbol.declarations??[]).map(h=>h.resolve(project)).filter(Boolean).map(key));
    const matchMemo=new Map();
    const matches=sym=>{
      const s=followAlias(sym);if(!s)return false;
      if(s.id===targetSymbol.id)return true;
      if(matchMemo.has(s.id))return matchMemo.get(s.id);
      const r=(s.declarations??[]).some(h=>declKeys.has(handleKey(h)));
      matchMemo.set(s.id,r);return r;
    };
    const searched=new Set(sources);
    if(!searched.has(target))searched.add(target);
    for(const sf of searched){
      const file=original.get(norm(sf.fileName));if(!file)continue;
      const nodes=identifiersNamed(sf,t=>t===name);
      if(!nodes.length)continue;
      let syms;
      try{syms=checker.getSymbolAtLocation(nodes);}catch(e){complete=false;diagnostics.push({kind:'references_file_failed',message:file+': '+e});continue;}
      nodes.forEach((node,i)=>{
        if(!syms[i]||!matches(syms[i]))return;
        const start=node.getStart(sf);
        const isDecl=!!node.parent&&node.parent.name===node&&declNodes.has(key(node.parent));
        const k=file+':'+bytes(sf,start);
        const prev=results.get(k);
        if(!prev||isDecl)results.set(k,{file,start_byte:bytes(sf,start),end_byte:bytes(sf,node.end),line:lineOf(sf,start),is_declaration:isDecl||!!prev?.is_declaration});
      });
    }
  }
  if(!found){complete=false;diagnostics.push({kind:'references_target_unresolved',message:'no checker symbol at the queried position'});}
  return {references:[...results.values()],references_complete:complete,diagnostics};
}

function declare(n,scope=[]){
  let next=scope;
  if((callable.has(n.kind)&&n.body)||n.kind===K.ClassDeclaration){const id=add(n,scope);if(id)next=[...scope,symbols[id].name];}
  n.forEachChild(c=>{declare(c,next);});
}
for(const g of groups)for(const sf of g.sources)declare(sf);
const assigned=assignedFunctions(groups,{K,SymbolFlags,declarations,key,fileOf,diagnostics});
for(const g of groups)analyzeGroup(g,assigned);
realLibraryFiles(ctx,library_files);
// Declared parameter types are the checker's display strings; an import type names the
// virtual workspace (`typeof import("<workspace>/src/x")`), which lives in trace's cache:
// report it relative to the repository so answers never depend on the cache location.
for(const p of callback_params){
  if(typeof p.param_type==='string')p.param_type=p.param_type.split(ctx.root+'/').join('');
}
diagnostics.push({kind:'typescript_project',message:'TypeScript 7 compiler API over the project configuration ('+groups.map(g=>g.config.startsWith(ctx.root+'/')?g.config.slice(ctx.root.length+1):g.config).join(', ')+'), installed node_modules (read only) and bundled Node types when the project has none; nothing is installed or run.'});
return {symbols,edges,unresolved,uses,diagnostics,callback_params,library_files,library_calls,metrics:{compiler:'typescript-7',files:groups.reduce((a,g)=>a+g.sources.length,0),projects:groups.length,timing:ctx.api.getTimingInfo()}};
} finally {snapshot.dispose();}

function aliasFollower(checker){
  const memo=new Map();
  return function followAlias(sym){
    if(!sym)return null;
    if(!(sym.flags&SymbolFlags.Alias))return sym;
    if(memo.has(sym.id))return memo.get(sym.id);
    let t=null;
    try{const a=checker.getAliasedSymbol(sym);t=a&&!checker.isUnknownSymbol(a)?a:null;}catch{t=null;}
    memo.set(sym.id,t);return t;
  };
}

function analyzeGroup({project,sources},assigned){
  const {checker}=project;
  // Library hooks (fntype.mjs, external.mjs) read the current project and checker from ctx.
  ctx.project=project;ctx.checker=checker;
  const followAlias=aliasFollower(checker);
  function target(n){
    const direct=declarations.get(key(n));if(direct)return direct;
    if(n.name){const symbol=checker.getSymbolAtLocation(n.name);const candidates=(symbol?.declarations??[]).map(h=>h.resolve(project)).filter(Boolean).map(d=>declarations.get(key(d))).filter(Boolean);if(new Set(candidates).size===1)return candidates[0];}
    return null;
  }
  // The repository class a symbol names.
  function classOf(sym){
    const s=followAlias(sym);if(!s||!(s.flags&SymbolFlags.Class))return {id:null,outside:false};
    for(const h of s.declarations??[]){
      const d=h.resolve(project);if(!d)continue;
      const id=declarations.get(key(d));if(id)return {id,outside:false};
      if(!fileOf(d))return {id:null,outside:true};
    }
    return {id:null,outside:false};
  }
  // A use target: the unique owner-declaration (function / class, or the function bound by a
  // variable / field / property initializer), else the unique declaration-only target.
  const useMemo=new Map();
  function useTarget(sym){
    const s=followAlias(sym);if(!s)return null;
    if(useMemo.has(s.id))return useMemo.get(s.id);
    const owners=new Set(), others=new Set();
    for(const h of s.declarations??[]){
      const d=h.resolve(project);if(!d)continue;
      const direct=declarations.get(key(d));
      if(direct){owners.add(direct);continue;}
      const init=d.initializer;
      if(init&&(callable.has(init.kind)||init.kind===K.ClassExpression)){const i=declarations.get(key(init));if(i){owners.add(i);continue;}}
      if(typeTargets.has(d.kind)&&d.name&&fileOf(d)){const id=symbolEntry(d,d.name.getText(d.getSourceFile()),[],'declaration');if(id)others.add(id);}
    }
    const r=owners.size===1?[...owners][0]:owners.size===0&&others.size===1?[...others][0]:null;
    useMemo.set(s.id,r);return r;
  }
  function evidenceOf(n,sf){return {file:original.get(norm(sf.fileName)),start_byte:bytes(sf,n.getStart(sf)),end_byte:bytes(sf,n.end),line:lineOf(sf,n.getStart(sf))};}
  // A binding whose value is loaded from a package: declared only outside the repository
  // (an import resolved into node_modules / a library's global types), an import from a
  // package specifier, or `x = require('pkg')` / `{a} = require('pkg')` / `x = require('pkg').y`.
  function boundToPackage(sym){
    if(!sym)return false;
    const raw=(sym.declarations??[]).map(h=>h.resolve(project)).filter(Boolean);
    const fromPackageImport=d=>{
      for(let a=d,i=0;a&&i<4;a=a.parent,i++){
        if(a.kind===K.ImportDeclaration)return isPackageSpecifier(a.moduleSpecifier?.text);
        if(a.kind===K.ImportEqualsDeclaration)return isPackageSpecifier(a.moduleReference?.expression?.text);
      }
      return false;
    };
    if(raw.length&&raw.every(fromPackageImport))return true;
    const s=followAlias(sym);if(!s)return false;
    const decls=(s.declarations??[]).map(h=>h.resolve(project)).filter(Boolean);
    if(!decls.length)return false;
    if(decls.every(d=>!fileOf(d)))return true;
    return decls.every(d=>{
      let v=d;
      while(v&&v.kind===K.BindingElement)v=v.parent?.parent;
      if(!v||v.kind!==K.VariableDeclaration||!v.initializer)return false;
      const init=chainRoot(v.initializer);
      return !!init&&init.kind===K.CallExpression&&isRequire(init)&&isPackageSpecifier(init.arguments[0].text);
    });
  }
  // An unknown call that certainly runs no repository code (checker facts + the repository's
  // declared names, no tables): the chain starts at a free global no repository declaration
  // names (`it(...)`, `describe(...)`: injected by the runtime or a test runner), at a value
  // loaded from a package (`request(app)` with `request = require('supertest')`), or at
  // `require('pkg')` itself; a member call on such a chain additionally needs a member name no
  // repository declaration has (`request(app).expect(200)`; `.get` stays unknown: the
  // repository declares a `get`).
  function outsideRepository(n,root,sym,sf){
    if(!root)return false;
    let fromPackage=false;
    if(root.kind===K.CallExpression)fromPackage=isRequire(root)&&isPackageSpecifier(root.arguments[0].text);
    else if(root.kind===K.Identifier)fromPackage=sym?boundToPackage(sym):!names.has(text(root,sf));
    if(!fromPackage)return false;
    if(calleeOf(n)===root)return true;
    const member=calleeName(n);
    return !!member&&member!==root&&!names.has(text(member,sf));
  }
  function visit(n,owner,found){
    const enclosingOwner=owner;
    if(declarations.has(key(n)) && n.kind!==K.ClassDeclaration)owner=declarations.get(key(n));
    if(owner && callKinds.has(n.kind))found.calls.push({node:n,owner});
    if((n.kind===K.Identifier||n.kind===K.PrivateIdentifier)&&names.size){
      const sf=n.getSourceFile();
      if(names.has(text(n,sf))&&!isCallee(n)){const kind=useKind(n);if(kind)found.uses.push({node:n,owner,kind});}
    }
    // Property reads that may run a getter.
    if(owner&&n.kind===K.PropertyAccessExpression&&getterNames.size&&!isCallee(n.name)){
      const sf=n.getSourceFile();const g=n.parent;
      if(getterNames.has(text(n.name,sf))&&!(isAssignment(g)&&g.left===n))found.gets.push({node:n,owner});
    }
    n.forEachChild(c=>{visit(c,c===n.name || c.kind===K.Decorator ? enclosingOwner : owner,found);});
  }
  function external(n,owner,evidence,declaration){
    unresolved.push({owner,kind:'external_signature',evidence});
    if(declaration){const lib=libraryCall(ctx,n,declaration,library_files);if(lib)library_calls.push(lib);}
    (n.arguments??[]).forEach((a,i)=>{
      if(!(a.kind===K.ArrowFunction||a.kind===K.FunctionExpression||a.kind===K.Identifier||a.kind===K.PropertyAccessExpression))return;
      const p=callbackParams(ctx,n,i);if(p)callback_params.push(p);
    });
  }
  for(const sf of sources){
    const file=original.get(norm(sf.fileName));
    if(queried&&!queried.has(file))continue;
    const found={calls:[],uses:[],gets:[]};
    visit(sf,moduleOwner(sf),found);
    // Calls: callee types in one batch, then the signature per call.
    let types=[];
    try{types=found.calls.length?checker.getTypeAtLocation(found.calls.map(c=>calleeOf(c.node))):[];}
    catch(e){diagnostics.push({kind:'calls_failed',message:file+': '+e});types=[];}
    const rules=[];
    // Unknown calls without a repository declaration, checked by `outsideRepository` below.
    const pending=[];
    const unknown=(n,owner,evidence,declaration)=>{
      if(declaration&&fileOf(declaration))unresolved.push({owner,kind:'unresolved_or_external_signature',evidence});
      else pending.push({n,owner,evidence});
    };
    found.calls.forEach(({node:n,owner},i)=>{
      const evidence=evidenceOf(n,sf);
      if(isRequire(n)){unresolved.push({owner,kind:'external_signature',evidence});return;}
      const type=types[i];
      const signature=checker.getResolvedSignature(n);const declaration=signature?.declaration?.resolve(project);
      const dest=declaration && target(declaration);
      if(dest && type && !(type.flags&(TypeFlags.Any|TypeFlags.Unknown|TypeFlags.Union))){
        let kind=n.kind===K.NewExpression?'constructor':'calls';
        if(symbols[dest].execution_model==='generator')kind=n.parent.kind===K.ForOfStatement||n.parent.kind===K.SpreadElement?'iterates':'creates_generator';
        edges.push({from:owner,to:dest,kind,evidence,resolution:'typescript_resolved_signature'});
        return;
      }
      // Checker rules that need the callee's symbol (resolved below in one batch).
      const name=calleeName(n);
      if(name&&!dest&&(n.kind===K.NewExpression?!declaration:true)){rules.push({n,owner,evidence,declaration,type,name});return;}
      // A call of a call (`compose(stack)(context)`): the function the inner call returns.
      let inner=n.kind===K.CallExpression?n.expression:null;
      while(inner&&inner.kind===K.ParenthesizedExpression)inner=inner.expression;
      const returned=!dest&&inner?.kind===K.CallExpression?assigned.returnedFunction(inner,project):null;
      if(returned){edges.push({from:owner,to:returned,kind:'calls',evidence,resolution:'typescript_returned_function'});return;}
      if(declaration && !dest && !fileOf(declaration) && type && !(type.flags&(TypeFlags.Any|TypeFlags.Unknown)))external(n,owner,evidence,declaration);
      else unknown(n,owner,evidence,declaration);
    });
    if(rules.length){
      let syms=[];
      try{syms=checker.getSymbolAtLocation(rules.map(r=>r.name));}catch(e){diagnostics.push({kind:'calls_failed',message:file+': '+e});syms=[];}
      rules.forEach(({n,owner,evidence,declaration,type},i)=>{
        const sym=syms[i];
        if(n.kind===K.NewExpression&&!declaration){
          const c=classOf(sym);
          if(c.id){edges.push({from:owner,to:c.id,kind:'constructor',evidence,resolution:'typescript_class_without_constructor'});return;}
          if(c.outside){external(n,owner,evidence,null);return;}
        } else {
          const held=sym?assigned.functionOf(followAlias(sym),project):null;
          if(held){edges.push({from:owner,to:held,kind:'calls',evidence,resolution:'typescript_assigned_function'});return;}
        }
        if(declaration && !fileOf(declaration) && type && !(type.flags&(TypeFlags.Any|TypeFlags.Unknown)))external(n,owner,evidence,declaration);
        else unknown(n,owner,evidence,declaration);
      });
    }
    if(pending.length){
      const roots=pending.map(p=>chainRoot(calleeOf(p.n)));
      const idents=roots.filter(r=>r&&r.kind===K.Identifier);
      let rsyms=[];
      try{rsyms=idents.length?checker.getSymbolAtLocation(idents):[];}
      catch(e){diagnostics.push({kind:'calls_failed',message:file+': '+e});rsyms=null;}
      let j=0;
      pending.forEach(({n,owner,evidence},i)=>{
        const root=roots[i];
        const sym=root&&root.kind===K.Identifier?(rsyms?rsyms[j++]:undefined):undefined;
        if(rsyms&&outsideRepository(n,root,sym,sf))external(n,owner,evidence,null);
        else unresolved.push({owner,kind:'unresolved_or_external_signature',evidence});
      });
    }
    // Getter reads.
    if(found.gets.length){
      let syms=[];
      try{syms=checker.getSymbolAtLocation(found.gets.map(g=>g.node.name));}catch(e){diagnostics.push({kind:'getters_failed',message:file+': '+e});syms=[];}
      found.gets.forEach(({node,owner},i)=>{
        const s=followAlias(syms[i]);if(!s||!(s.flags&SymbolFlags.GetAccessor))return;
        const getters=new Set();
        for(const h of s.declarations??[]){const d=h.resolve(project);if(d?.kind===K.GetAccessor){const id=declarations.get(key(d));if(id)getters.add(id);}}
        if(getters.size===1)edges.push({from:owner,to:[...getters][0],kind:'property_get',evidence:evidenceOf(node,sf),resolution:'typescript_get_accessor'});
      });
    }
    // Uses of declared names.
    if(!found.uses.length)continue;
    let syms;
    try{syms=checker.getSymbolAtLocation(found.uses.map(p=>p.node));}
    catch(e){diagnostics.push({kind:'uses_failed',message:(file??sf.fileName)+': '+e});continue;}
    found.uses.forEach((p,i)=>{
      const to=syms[i]?useTarget(syms[i]):null;if(!to)return;
      const start=p.node.getStart(sf);
      uses.push({from:p.owner,to,kind:p.kind,evidence:{file,start_byte:bytes(sf,start),end_byte:bytes(sf,p.node.end),line:lineOf(sf,start)}});
    });
  }
}
}
