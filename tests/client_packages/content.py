"""Instruction and asset checks for the authored package's finite surfaces."""
from pathlib import Path
import re
import xml.etree.ElementTree as ET

from files import (InvalidPackage, contained, fields, frontmatter, metadata,
                   read_text, require, text_value)


WORKFLOWS = ('ask', 'build', 'fix', 'fix-ci', 'review', 'status')
TOOLS = {'session_create', 'session_list', 'session_get', 'session_send', 'session_cancel'}
SKILL = 'skills/foundry/SKILL.md'


def links(package, path, body):
    references = re.findall(r'\[[^\]]*\]\(([^\s)]+)(?:\s+"[^"]*")?\)', body)
    resolved = set()
    for reference in references:
        if reference.startswith(('https://', 'http://', '#')):
            continue
        reference = reference.split('#', 1)[0]
        resolved.add(contained(package, reference, base=path.parent, parents=True))
    return resolved


def tool_names(text):
    # Only inline-code operation references, not IDs or arbitrary English prose.
    names = set(re.findall(r'`(session_[a-z_]+)`', text)) - {'session_id'}
    require(names <= TOOLS, 'unsupported callable tool reference')
    return names


def svg(path):
    text = read_text(path)
    require('<!DOCTYPE' not in text and '<!ENTITY' not in text, 'external SVG entity')
    try:
        root = ET.fromstring(text)
    except ET.ParseError as error:
        raise InvalidPackage('invalid SVG') from error
    require(root.tag == '{http://www.w3.org/2000/svg}svg', 'SVG namespace required')
    try:
        box = [float(value) for value in root.attrib['viewBox'].split()]
    except (KeyError, ValueError) as error:
        raise InvalidPackage('SVG viewBox required') from error
    require(len(box) == 4 and box[:2] == [0, 0] and 0 < box[2] == box[3] <= 4096,
            'square bounded SVG required')
    allowed = {'svg', 'title', 'desc', 'metadata', 'rect', 'path', 'circle', 'g', 'polygon',
               'line', 'polyline', 'ellipse'}
    require(root.find('{http://www.w3.org/2000/svg}title') is not None and
            root.find('{http://www.w3.org/2000/svg}desc') is not None, 'SVG accessible text required')
    backgrounds = root.findall('{http://www.w3.org/2000/svg}rect')
    require(any(rect.get('width') == str(int(box[2])) and
                rect.get('height') == str(int(box[3])) and
                re.fullmatch(r'#[0-9a-fA-F]{6}', rect.get('fill', '')) and
                rect.get('opacity', '1') == '1' for rect in backgrounds),
            'opaque square SVG background required')
    for element in root.iter():
        require(element.tag.removeprefix('{http://www.w3.org/2000/svg}') in allowed,
                'active or external SVG element')
        for key, value in element.attrib.items():
            require(not key.lower().startswith('on') and
                    not key.endswith('href') and 'url(' not in value.lower(),
                    'external or active SVG attribute')


def validate_content(package, documents):
    skill_path = contained(package, SKILL)
    header, skill = frontmatter(skill_path)
    fields(header, ('name', 'description'))
    require(header['name'] == 'foundry', 'skill identity')
    text_value(header['description'])
    require(tool_names(skill) == TOOLS, 'callable tool inventory incomplete')
    # These checks guard critical explicit limits; prose still requires human review.
    for token in ('idempotency_key', 'message_id', 'delivery_not_confirmed',
                  'not_confirmed', 'action_required', 'graceful', 'hard'):
        require(token in skill, 'missing lifecycle or delivery guidance')
    require('queues' in skill and 'neither starts nor resumes' in skill,
            'queueing must not promise execution')
    require('Package validation does not' in skill, 'missing static-validation boundary')
    paths = []
    for workflow in WORKFLOWS:
        path = contained(package, f'commands/f5y-{workflow}.md')
        header, body = frontmatter(path)
        fields(header, ('description',))
        text_value(header['description'])
        require(skill_path in links(package, path, body), 'workflow must link coordination skill')
        require(len(body.splitlines()) >= 8, 'workflow lacks task instructions')
        tool_names(body)
        paths.append(path)
    require(set((package / 'commands').glob('*.md')) == set(paths), 'unexpected workflow set')
    rule_path = contained(package, 'rules/foundry-handoff.mdc')
    rule, body = frontmatter(rule_path)
    fields(rule, ('description', 'alwaysApply'))
    text_value(rule['description'])
    require(rule['alwaysApply'] is False, 'handoff must be opt-in')
    require(skill_path in links(package, rule_path, body), 'rule must link coordination skill')
    agent_path = contained(package, 'skills/foundry/agents/openai.yaml')
    agent = metadata(read_text(agent_path))
    fields(agent, ('interface',))
    fields(agent['interface'], ('display_name', 'short_description', 'default_prompt'),
           ('icon_small', 'icon_large', 'brand_color'))
    for key in ('display_name', 'short_description', 'default_prompt'):
        text_value(agent['interface'][key])
    require('$foundry' in agent['interface']['default_prompt'], 'skill invocation missing')
    for path in package.rglob('*'):
        if path.suffix in ('.md', '.mdc', '.yaml', '.json'):
            text = read_text(path)
            require('${' not in text and '{{' not in text, 'undeclared package variable')
            if path.suffix in ('.md', '.mdc'):
                links(package, path, text)
    for key in ('icon_small', 'icon_large'):
        if key in agent['interface']:
            contained(package, agent['interface'][key], base=package / 'skills/foundry')
    for document in documents:
        for key in ('logo', 'icon'):
            if key in document:
                contained(package, document[key])
    assets = [contained(package, f'assets/{name}.svg')
              for name in ('logo-light', 'logo-dark', 'skill-light', 'skill-dark')]
    for path in assets:
        svg(path)
