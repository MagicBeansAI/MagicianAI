import importlib.machinery
import importlib.util
import pathlib
import unittest
from unittest.mock import patch
import contextlib
import io
import sys

path = pathlib.Path(__file__).parents[1] / 'bin/calendar-adapter'
loader = importlib.machinery.SourceFileLoader('calendar_adapter', str(path))
spec = importlib.util.spec_from_loader(loader.name, loader)
adapter = importlib.util.module_from_spec(spec)
loader.exec_module(adapter)


class CalendarAdapterTests(unittest.TestCase):
    def request(self, action='create_event'):
        return dict(action=action, calendar_id='primary', start='2026-10-14T10:00:00Z', end='2026-10-14T11:00:00Z', attendees=['Guest@Example.test'], summary='Reviewed meeting', idempotency_key='same-booking', event_id='event-1')

    def test_create_has_stable_provider_id_and_confirmed_resource_facts(self):
        writes = []
        def transport(method, params, body=None, **options):
            if method == ['calendars', 'get']: return {'id':'company@example.test'}
            writes.append((params, body))
            return {**body, 'etag':'revision-1'}
        one = adapter.execute(self.request(), transport)
        two = adapter.execute(self.request(), transport)
        self.assertEqual(one['provider_receipt_id'], two['provider_receipt_id'])
        self.assertEqual(writes[0][1]['id'], writes[1][1]['id'])
        self.assertEqual(one['resource_effect']['attributes']['attendees'], ['guest@example.test'])
        self.assertEqual(one['resource_effect']['operation'], 'create')

    def test_existing_event_recipient_change_is_refused_before_any_write(self):
        calls = []
        def transport(method, params, body=None, **options):
            calls.append(method)
            if method == ['calendars','get']: return {'id':'company@example.test'}
            return {'id':'event-1','etag':'revision-1','attendees':[{'email':'other@example.test'}]}
        for action in ('update_event','delete_event'):
            with self.assertRaisesRegex(ValueError, 'attendees'):
                adapter.execute(self.request(action), transport)
        self.assertNotIn(['events','patch'], calls)
        self.assertNotIn(['events','delete'], calls)

    def test_release_and_update_use_the_actual_revision_as_a_precondition(self):
        writes = []
        def transport(method, params, body=None, **options):
            if method == ['calendars','get']: return {'id':'company@example.test'}
            current = dict(id='event-1',etag='revision-1',attendees=[{'email':'guest@example.test'}])
            if method == ['events','get']: return current
            writes.append(options)
            return {**current, **(body or {}), 'etag':'revision-2'}
        update = adapter.execute(self.request('update_event'), transport)
        release = adapter.execute(self.request('delete_event'), transport)
        self.assertEqual(writes[0]['if_match'], 'revision-1')
        self.assertEqual(writes[1]['if_match'], 'revision-1')
        self.assertNotEqual(update['provider_receipt_id'], release['provider_receipt_id'])
        self.assertEqual(release['resource_effect']['operation'], 'delete')

    def test_unknown_availability_is_not_free(self):
        def transport(method, params, body=None, **options):
            if method == ['calendars','get']: return {'id':'company@example.test'}
            return {'calendars':{'company@example.test':{'errors':[{'reason':'notFound'}]}}}
        with self.assertRaisesRegex(ValueError, 'unknown'):
            adapter.execute(self.request('free_busy'), transport)

    def test_booking_collision_requires_identical_frozen_content(self):
        def transport(method, params, body=None, **options):
            if method == ['calendars','get']: return {'id':'company@example.test'}
            if method == ['events','insert']: raise adapter.CalendarError(409)
            return {'extendedProperties':{'private':{'magician_request':'different-request'}}}
        with self.assertRaisesRegex(ValueError, 'different request'):
            adapter.execute(self.request(), transport)

    def test_offsets_and_fractional_instants_are_compared_as_times(self):
        with self.assertRaises(ValueError):
            adapter.valid_interval(adapter.instant('2026-10-14T10:00:00.1Z'), adapter.instant('2026-10-14T11:00:00+01:00'))
        with self.assertRaisesRegex(ValueError, 'UTC offset'):
            adapter.instant('2026-10-14T10:00:00')

    def test_typed_argv_keeps_one_hundred_attendees_without_interpreting_them_as_flags(self):
        attendees = [f'guest{i}@example.test' for i in range(100)]
        args = ['calendar-adapter','create_event','--start','2026-10-14T10:00:00Z','--end','2026-10-14T11:00:00Z','--summary','Reviewed meeting','--idempotency-key','booking','--',*attendees]
        with patch.object(sys,'argv',args), patch.object(adapter,'execute',return_value={}) as execute, contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(adapter.main(),0)
            self.assertEqual(execute.call_args.args[0]['attendees'],attendees)

    def test_both_reviewed_profiles_use_the_same_adapter_source(self):
        self.assertEqual(path.read_bytes(), (path.parents[2] / 'presto-calendar/bin/calendar-adapter').read_bytes())


if __name__ == '__main__': unittest.main()
